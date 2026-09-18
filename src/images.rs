//! Inline images: a line that is only `![alt](file.png)` (or `![[file.png]]`)
//! gets the picture drawn underneath it.
//!
//! In terminals with the Kitty graphics protocol the image is sent once and
//! then placed with Unicode placeholder cells. Those are ordinary characters as
//! far as the rest of the app is concerned, so scrolling, clipping and redraws
//! need no special handling. Everywhere else the image is approximated with
//! half-block characters, two pixels per cell.
//!
//! `https://` images are fetched on a background thread, after the URL has
//! stopped changing for a moment, and kept in `~/.cache/omanote/images`.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};
use ratatui::style::{Color, Style};

use crate::clipboard::{base64, unbase64};
use crate::diacritics::DIACRITICS;
use crate::layout::VRow;
use crate::markdown::{REF, definition, image_line, marker};

const PLACEHOLDER: char = '\u{10EEEE}';
const CHUNK: usize = 4096;
const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
/// A URL has to sit unchanged this long before we fetch it, so editing one
/// does not fire a request per keystroke.
const SETTLE: Duration = Duration::from_millis(700);
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Kitty,
    Blocks,
    Off,
}

/// Guess from the environment; asking the terminal would mean parsing its
/// reply out of the key stream. `OMANOTE_IMAGES=kitty|blocks|off` overrides.
pub fn detect() -> Mode {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    match var("OMANOTE_IMAGES").as_str() {
        "kitty" => return Mode::Kitty,
        "blocks" => return Mode::Blocks,
        "off" => return Mode::Off,
        _ => {}
    }
    let term = var("TERM");
    let native = var("TERM_PROGRAM") == "ghostty"
        || term.contains("ghostty")
        || term.contains("kitty")
        || !var("KITTY_WINDOW_ID").is_empty();
    // tmux swallows graphics escapes unless passthrough is set up.
    if native && var("TMUX").is_empty() { Mode::Kitty } else { Mode::Blocks }
}

type Lead = Vec<(String, Style)>;

enum Source {
    /// A URL first seen at this moment, not requested yet.
    Settling(Instant),
    Loading,
    Ready(DynamicImage),
    Failed(String),
}

type Fetched = (String, Result<DynamicImage, String>);

struct Entry {
    id: u32,
    source: Source,
    /// Rows to draw, one `Lead` each, for the box they were last fitted to.
    fitted: Vec<Lead>,
    cols: u16,
}

pub struct Images {
    mode: Mode,
    /// Pixel size of one terminal cell.
    cell: (u32, u32),
    dirs: Vec<PathBuf>,
    entries: HashMap<String, Entry>,
    /// `![alt][label]` → the entry it resolves to (an embed, or a `[label]: file` definition).
    refs: HashMap<String, String>,
    next_id: u32,
    fitted_for: (u16, u16),
    stale: bool,
    outbox: Vec<u8>,
    /// `None` turns remote images off (`OMANOTE_REMOTE_IMAGES=off`).
    cache_dir: Option<PathBuf>,
    settle: Duration,
    fetched: (Sender<Fetched>, Receiver<Fetched>),
}

impl Images {
    pub fn new(mode: Mode, cell: (u16, u16), note: Option<&Path>, vault: PathBuf) -> Self {
        let note_dir = note.and_then(Path::parent).map(Path::to_path_buf).filter(|d| !d.as_os_str().is_empty());
        let mut images = Images {
            mode,
            cell: (cell.0.max(1) as u32, cell.1.max(1) as u32),
            dirs: vec![note_dir.unwrap_or_else(|| PathBuf::from(".")), vault],
            entries: HashMap::new(),
            refs: HashMap::new(),
            next_id: 1,
            fitted_for: (0, 0),
            stale: true,
            outbox: Vec::new(),
            cache_dir: cache_dir(),
            settle: SETTLE,
            fetched: channel(),
        };
        if mode == Mode::Kitty {
            // Forget whatever a previous note left in the terminal.
            images.outbox.extend(b"\x1b_Ga=d,d=A,q=2\x1b\\");
        }
        images
    }

    pub fn off() -> Self {
        Images::new(Mode::Off, (10, 20), None, PathBuf::new())
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// The note got a file (or moved): relative image paths resolve from there now.
    pub fn set_note(&mut self, note: &Path, vault: PathBuf) {
        let dir = note.parent().map(Path::to_path_buf).filter(|d| !d.as_os_str().is_empty());
        self.dirs = vec![dir.unwrap_or_else(|| PathBuf::from(".")), vault];
        self.stale = true;
    }

    pub fn set_cell(&mut self, cell: (u16, u16)) {
        let cell = (cell.0.max(1) as u32, cell.1.max(1) as u32);
        if cell != self.cell {
            self.cell = cell;
            self.fitted_for = (0, 0);
        }
    }

    /// Escape sequences that must reach the terminal before the next frame.
    pub fn take_outbox(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outbox)
    }

    fn resolve(&self, target: &str) -> Option<PathBuf> {
        let target = target.replace("%20", " ");
        if let Some(rest) = target.strip_prefix("~/") {
            return Some(PathBuf::from(std::env::var_os("HOME")?).join(rest));
        }
        let path = Path::new(&target);
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        self.dirs.iter().map(|d| d.join(path)).find(|p| p.exists())
    }

    /// Load new images, drop ones no longer mentioned, and refit to the text column.
    pub fn prepare(&mut self, lines: &[Vec<char>], embeds: &[(String, String)], width: u16, max_rows: u16) {
        if self.mode == Mode::Off {
            return;
        }
        // Downloads that finished since the last frame.
        while let Ok((target, result)) = self.fetched.1.try_recv() {
            if let Some(entry) = self.entries.get_mut(&target) {
                entry.source = result.map_or_else(Source::Failed, Source::Ready);
                self.fit(&target, width, max_rows);
            }
        }
        let settling = self.entries.values().any(|e| matches!(e.source, Source::Settling(_)));
        if !self.stale && !settling && self.fitted_for == (width, max_rows) {
            return;
        }
        let refit = self.fitted_for != (width, max_rows);
        self.stale = false;
        self.fitted_for = (width, max_rows);

        // Reference-style images point at an embedded image or at a `[label]: target` line.
        let defs: HashMap<String, String> = lines.iter().filter_map(|l| definition(l)).collect();
        self.refs.clear();
        let mut wanted: Vec<String> = Vec::new();
        for target in lines.iter().filter_map(|l| image_line(l)) {
            let key = match target.strip_prefix(REF) {
                Some(label) if embeds.iter().any(|(l, _)| l == label) => target.clone(),
                Some(label) => defs.get(label).cloned().unwrap_or_else(|| target.clone()),
                None => target.clone(),
            };
            if target.starts_with(REF) {
                self.refs.insert(target, key.clone());
            }
            if !wanted.contains(&key) {
                wanted.push(key);
            }
        }
        let gone: Vec<String> = self.entries.keys().filter(|k| !wanted.contains(k)).cloned().collect();
        for target in gone {
            if let Some(entry) = self.entries.remove(&target) {
                self.delete(entry.id);
            }
        }
        for target in wanted {
            if !self.entries.contains_key(&target) {
                let source = if let Some(label) = target.strip_prefix(REF) {
                    match embeds.iter().find(|(l, _)| l == label) {
                        Some((_, uri)) => decode_data_uri(uri).map_or_else(Source::Failed, Source::Ready),
                        None => Source::Failed(format!("nothing defines [{label}]")),
                    }
                } else if is_url(&target) {
                    Source::Settling(Instant::now())
                } else {
                    match self.resolve(&target) {
                        Some(path) => image::open(&path).map_or_else(|e| Source::Failed(format!("{target}: {e}")), Source::Ready),
                        None => Source::Failed(format!("{target}: file not found")),
                    }
                };
                let id = self.next_id;
                self.next_id += 1;
                self.entries.insert(target.clone(), Entry { id, source, fitted: Vec::new(), cols: 0 });
            }
            if matches!(self.entries[&target].source, Source::Settling(since) if since.elapsed() >= self.settle) {
                self.start_fetch(&target);
            } else if self.entries[&target].fitted.is_empty() || refit {
                self.fit(&target, width, max_rows);
            }
        }
    }

    /// From the disk cache if we have it, otherwise over the network on its own thread.
    fn start_fetch(&mut self, url: &str) {
        let Some(entry) = self.entries.get_mut(url) else { return };
        let Some(dir) = &self.cache_dir else {
            entry.source = Source::Failed("remote images are turned off".into());
            return;
        };
        let cached = dir.join(format!("{:016x}", fnv1a(url)));
        if let Ok(img) = std::fs::read(&cached).map_err(drop).and_then(|b| image::load_from_memory(&b).map_err(drop)) {
            entry.source = Source::Ready(img);
            self.stale = true;
            let (w, r) = self.fitted_for;
            return self.fit(url, w, r);
        }
        entry.source = Source::Loading;
        let (tx, url) = (self.fetched.0.clone(), url.to_string());
        std::thread::spawn(move || {
            let result = download(&url, &cached);
            let _ = tx.send((url, result));
        });
    }

    fn delete(&mut self, id: u32) {
        if self.mode == Mode::Kitty {
            self.outbox.extend(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").bytes());
        }
    }

    fn fit(&mut self, target: &str, width: u16, max_rows: u16) {
        let (cw, ch) = self.cell;
        let Some(entry) = self.entries.get_mut(target) else { return };
        let Source::Ready(img) = &entry.source else { return };
        let (iw, ih) = img.dimensions();
        let (iw, ih) = (iw.max(1) as f64, ih.max(1) as f64);

        // Natural size, shrunk to the column, then to the height cap, keeping the aspect.
        let mut cols = (iw / cw as f64).ceil().clamp(1.0, width.max(1) as f64);
        let mut rows = (cols * cw as f64 * ih / iw / ch as f64).ceil().max(1.0);
        let cap = max_rows.clamp(1, DIACRITICS.len() as u16) as f64;
        if rows > cap {
            rows = cap;
            cols = (rows * ch as f64 * iw / ih / cw as f64).round().clamp(1.0, width.max(1) as f64);
        }
        let (cols, rows) = (cols as u16, rows as u16);
        entry.cols = cols;

        match self.mode {
            Mode::Kitty => {
                let (pw, ph) = (cols as u32 * cw, rows as u32 * ch);
                let scaled = if (pw as f64) < iw || (ph as f64) < ih { img.resize(pw, ph, FilterType::Triangle) } else { img.clone() };
                let mut png = Vec::new();
                if scaled.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).is_err() {
                    return;
                }
                let id = entry.id;
                entry.fitted = placeholder_rows(id, cols, rows);
                let payload = base64(&png);
                let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
                let mut first = true;
                self.outbox.extend(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").bytes());
                while let Some(chunk) = chunks.next() {
                    let more = chunks.peek().is_some() as u8;
                    let head = if first { format!("a=T,U=1,f=100,i={id},c={cols},r={rows},q=2,m={more}") } else { format!("m={more}") };
                    first = false;
                    self.outbox.extend(b"\x1b_G");
                    self.outbox.extend(head.bytes());
                    self.outbox.push(b';');
                    self.outbox.extend(chunk);
                    self.outbox.extend(b"\x1b\\");
                }
            }
            Mode::Blocks => entry.fitted = block_rows(img, cols, rows),
            Mode::Off => {}
        }
    }

    /// Virtual rows to draw under an image line.
    pub fn rows_for(&self, chars: &[char]) -> Vec<VRow> {
        if self.mode == Mode::Off {
            return Vec::new();
        }
        let Some(target) = image_line(chars) else { return Vec::new() };
        let Some(entry) = self.entries.get(self.refs.get(&target).unwrap_or(&target)) else { return Vec::new() };
        let virt = |lead: Lead, lead_w: u16| VRow { lead, lead_w, cells: Vec::new(), start: 0, end: 0, last: false, virt: true };
        match &entry.source {
            Source::Settling(_) | Source::Loading => vec![virt(vec![("  ↓ loading image…".to_string(), marker())], 0)],
            Source::Failed(msg) => vec![virt(vec![(format!("  ⚠ {msg}"), marker())], 0)],
            Source::Ready(_) => entry.fitted.iter().map(|lead| virt(lead.clone(), entry.cols)).collect(),
        }
    }
}

fn is_url(target: &str) -> bool {
    let lower = target.to_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Is this `![..](target)` a picture we can draw? Any URL counts (many have no
/// extension); a local target needs an image extension, so `![[a note]]` does not.
pub fn is_image_path(target: &str) -> bool {
    let lower = target.to_lowercase();
    is_url(target) || EXTENSIONS.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

fn decode_data_uri(uri: &str) -> Result<DynamicImage, String> {
    let data = uri.split_once(";base64,").map(|(_, d)| d).ok_or("embedded image is not base64")?;
    let bytes = unbase64(data).ok_or("embedded image data is damaged")?;
    image::load_from_memory(&bytes).map_err(|e| format!("embedded image cannot be read: {e}"))
}

/// Get a pasted picture ready to embed: anything wider than `max_width` is
/// scaled down (a 4K screenshot is megabytes of base64 otherwise). Returns the
/// data URI.
pub fn embeddable(mime: &str, bytes: Vec<u8>, max_width: u32) -> Result<String, String> {
    let img = image::load_from_memory(&bytes).map_err(|e| format!("clipboard image cannot be read: {e}"))?;
    if img.width() <= max_width {
        return Ok(format!("data:{mime};base64,{}", base64(&bytes)));
    }
    let scaled = img.resize(max_width, u32::MAX, FilterType::Lanczos3);
    let mut png = Vec::new();
    scaled.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(format!("data:image/png;base64,{}", base64(&png)))
}

fn cache_dir() -> Option<PathBuf> {
    if std::env::var("OMANOTE_REMOTE_IMAGES").is_ok_and(|v| v == "off") {
        return None;
    }
    let base = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = base.or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache")))?;
    Some(base.join("omanote").join("images"))
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn download(url: &str, cache: &Path) -> Result<DynamicImage, String> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).redirects(5).build();
    let response = agent.get(url).set("User-Agent", concat!("omanote/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the server answered HTTP {code}"),
        // The full transport error repeats the URL, which is already on the line above.
        ureq::Error::Transport(t) => format!("could not download ({})", t.kind()),
    })?;
    let mut bytes = Vec::new();
    response.into_reader().take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| format!("download interrupted: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("image is larger than 25 MB".into());
    }
    let img = image::load_from_memory(&bytes).map_err(|e| format!("not an image we can read: {e}"))?;
    if let Some(dir) = cache.parent() {
        let _ = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(cache, &bytes));
    }
    Ok(img)
}

/// One string of placeholder cells per row. The image id rides in the
/// foreground colour; two combining marks per cell carry its row and column.
fn placeholder_rows(id: u32, cols: u16, rows: u16) -> Vec<Lead> {
    let style = Style::new().fg(Color::Rgb((id >> 16) as u8, (id >> 8) as u8, id as u8));
    (0..rows as usize)
        .map(|r| {
            let mut text = String::with_capacity(cols as usize * 10);
            for c in 0..(cols as usize).min(DIACRITICS.len()) {
                text.extend([PLACEHOLDER, DIACRITICS[r], DIACRITICS[c]]);
            }
            vec![(text, style)]
        })
        .collect()
}

/// `▀` per cell: the top pixel as foreground, the bottom one as background.
fn block_rows(img: &DynamicImage, cols: u16, rows: u16) -> Vec<Lead> {
    let small = img.resize_exact(cols as u32, rows as u32 * 2, FilterType::Triangle).to_rgba8();
    let colour = |x: u32, y: u32| {
        let p = small.get_pixel(x, y).0;
        (p[3] >= 128).then_some(Color::Rgb(p[0], p[1], p[2]))
    };
    (0..rows as u32)
        .map(|r| {
            (0..cols as u32)
                .map(|x| match (colour(x, r * 2), colour(x, r * 2 + 1)) {
                    (None, None) => (" ".to_string(), Style::default()),
                    (Some(top), None) => ("▀".to_string(), Style::new().fg(top)),
                    (None, Some(bottom)) => ("▄".to_string(), Style::new().fg(bottom)),
                    (Some(top), Some(bottom)) => ("▀".to_string(), Style::new().fg(top).bg(bottom)),
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(name: &str, w: u32, h: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omanote-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let img = image::RgbaImage::from_fn(w, h, |x, _| if x < w / 2 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 255, 255]) });
        img.save(&path).unwrap();
        path
    }

    fn lines(src: &str) -> Vec<Vec<char>> {
        src.lines().map(|l| l.chars().collect()).collect()
    }

    #[test]
    fn fits_to_width_and_height_keeping_aspect() {
        let path = write_png("wide.png", 800, 400);
        let doc = lines(&format!("![wide]({})", path.display()));
        let mut images = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());

        images.prepare(&doc, &[], 40, 30);
        let rows = images.rows_for(&doc[0]);
        assert_eq!((rows[0].lead_w, rows.len()), (40, 10), "800px is 80 cells, capped to the 40-wide column");
        assert!(rows.iter().all(|r| r.virt && r.lead.len() == 40));

        images.prepare(&doc, &[], 40, 4);
        let rows = images.rows_for(&doc[0]);
        assert_eq!((rows[0].lead_w, rows.len()), (16, 4), "height cap shrinks the width to match");

        // Left half red, right half blue.
        assert_eq!(rows[0].lead[0].1.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(rows[0].lead[15].1.fg, Some(Color::Rgb(0, 0, 255)));
    }

    #[test]
    fn kitty_sends_the_image_once_and_draws_placeholders() {
        let path = write_png("small.png", 40, 40);
        let doc = lines(&format!("text\n![]({})\nmore", path.display()));
        let mut images = Images::new(Mode::Kitty, (10, 20), None, PathBuf::new());
        images.prepare(&doc, &[], 60, 20);
        let out = String::from_utf8(images.take_outbox()).unwrap();
        assert!(out.contains("a=T,U=1,f=100,i=1,c=4,r=2,q=2,m=0;"), "{}", &out[..out.len().min(120)]);
        assert!(out.ends_with("\x1b\\"));

        let rows = images.rows_for(&doc[1]);
        assert_eq!(rows.len(), 2);
        let cells: Vec<char> = rows[1].lead[0].0.chars().collect();
        assert_eq!(cells.len(), 4 * 3);
        assert_eq!(&cells[3..6], [PLACEHOLDER, DIACRITICS[1], DIACRITICS[1]], "row 1, column 1");
        assert_eq!(rows[1].lead[0].1.fg, Some(Color::Rgb(0, 0, 1)), "image id 1");

        images.prepare(&doc, &[], 60, 20);
        assert!(images.take_outbox().is_empty(), "nothing changed, nothing resent");

        // Removing the line frees the image in the terminal.
        images.mark_stale();
        images.prepare(&lines("text\nmore"), &[], 60, 20);
        assert_eq!(String::from_utf8(images.take_outbox()).unwrap(), "\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    }

    /// Serve one PNG over plain HTTP on localhost, so the real download path runs offline.
    fn serve_once(body: Vec<u8>, status: &'static str) -> String {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pic", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&body));
        });
        url
    }

    fn wait_for(images: &mut Images, doc: &[Vec<char>], done: impl Fn(&[VRow]) -> bool) -> Vec<VRow> {
        for _ in 0..200 {
            images.prepare(doc, &[], 40, 30);
            let rows = images.rows_for(&doc[0]);
            if done(&rows) {
                return rows;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn remote_images_settle_download_and_cache() {
        let png = std::fs::read(write_png("remote.png", 100, 100)).unwrap();
        let url = serve_once(png, "200 OK");
        let doc = lines(&format!("![remote]({url})"));
        let cache = std::env::temp_dir().join(format!("omanote-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache);

        let mut images = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        images.cache_dir = Some(cache.clone());
        images.settle = Duration::from_millis(60);
        images.prepare(&doc, &[], 40, 30);
        assert!(images.rows_for(&doc[0])[0].lead[0].0.contains("loading"), "placeholder while the URL settles");
        assert!(matches!(images.entries[&url].source, Source::Settling(_)), "no request yet");

        let rows = wait_for(&mut images, &doc, |rows| rows.len() > 1);
        assert_eq!((rows[0].lead_w, rows.len()), (10, 5));

        // The server is gone now; a fresh store must get it from disk.
        let mut again = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        again.cache_dir = Some(cache.clone());
        again.settle = Duration::ZERO;
        assert_eq!(wait_for(&mut again, &doc, |rows| rows.len() > 1).len(), 5);
        let _ = std::fs::remove_dir_all(cache);
    }

    #[test]
    fn remote_failures_are_reported_not_fatal() {
        let url = serve_once(b"nope".to_vec(), "404 Not Found");
        let doc = lines(&format!("![gone]({url})"));
        let mut images = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        images.cache_dir = Some(std::env::temp_dir().join("omanote-cache-unused"));
        images.settle = Duration::ZERO;
        let rows = wait_for(&mut images, &doc, |rows| !rows[0].lead[0].0.contains("loading"));
        assert!(rows[0].lead[0].0.contains("HTTP 404"), "{}", rows[0].lead[0].0);

        let mut off = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        off.cache_dir = None;
        off.settle = Duration::ZERO;
        let rows = wait_for(&mut off, &doc, |rows| !rows[0].lead[0].0.contains("loading"));
        assert!(rows[0].lead[0].0.contains("turned off"));
    }

    #[test]
    fn reference_images_come_from_embeds_or_definitions() {
        let path = write_png("ref.png", 100, 100);
        let uri = embeddable("image/png", std::fs::read(&path).unwrap(), 2000).unwrap();
        assert!(uri.starts_with("data:image/png;base64,iVBOR"));
        let embeds = vec![("img1".to_string(), uri)];
        let doc = lines(&format!("![pasted image][img1]\n![by definition][Pic]\n![orphan][nope]\n[pic]: {}", path.display()));

        let mut images = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        images.prepare(&doc, &embeds, 40, 30);
        assert_eq!(images.rows_for(&doc[0]).len(), 5, "decoded from the data URI");
        assert_eq!(images.rows_for(&doc[1]).len(), 5, "resolved through the [pic]: line");
        assert!(images.rows_for(&doc[2])[0].lead[0].0.contains("nothing defines [nope]"));
        assert!(images.rows_for(&doc[3]).is_empty());
    }

    #[test]
    fn big_pastes_are_scaled_down() {
        let path = write_png("huge.png", 3000, 300);
        let uri = embeddable("image/png", std::fs::read(path).unwrap(), 1000).unwrap();
        let img = decode_data_uri(&uri).unwrap();
        assert_eq!((img.width(), img.height()), (1000, 100));
        assert!(embeddable("image/png", b"junk".to_vec(), 1000).is_err());
    }

    #[test]
    fn missing_files_say_so() {
        let doc = lines("![x](nope/missing.png)");
        let mut images = Images::new(Mode::Blocks, (10, 20), None, PathBuf::new());
        images.prepare(&doc, &[], 40, 10);
        let rows = images.rows_for(&doc[0]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].lead[0].0.contains("file not found"));
        assert!(images.rows_for(&lines("plain text")[0]).is_empty());
    }
}
