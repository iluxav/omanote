//! Files opened lately, wherever they are: `~/.omanote/recent.txt`.
//!
//! Notes in a vault are always found by Ctrl+P. A config file opened with
//! `omanote ~/.config/hypr/hyprland.lua` is not in a vault, so it is
//! remembered here instead, and Ctrl+P and `omanote <name>` find it again.
//! One line per file, the newest first: seconds since 1970, a tab, the path.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Enough for every config file anyone edits; the oldest fall off the end.
const KEEP: usize = 300;

pub fn file(home: &Path) -> PathBuf {
    home.join("recent.txt")
}

/// The files, newest first, with when each was last opened. Files that are
/// gone are left out.
pub fn list(home: &Path) -> Vec<(PathBuf, SystemTime)> {
    read(home).into_iter().filter(|(p, _)| p.is_file()).collect()
}

fn read(home: &Path) -> Vec<(PathBuf, SystemTime)> {
    let text = std::fs::read_to_string(file(home)).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (secs, path) = line.split_once('\t')?;
            let at = UNIX_EPOCH + Duration::from_secs(secs.trim().parse().ok()?);
            Some((PathBuf::from(path), at))
        })
        .filter(|(p, _)| p.is_absolute())
        .collect()
}

/// `path` was just opened: it goes to the top. Best effort: a list that
/// cannot be written is no reason to stop anyone editing.
pub fn opened(home: &Path, path: &Path) {
    let Ok(path) = std::path::absolute(path) else { return };
    let path = path.canonicalize().unwrap_or(path);
    let mut all = read(home);
    if all.first().is_some_and(|(p, _)| *p == path) {
        return;
    }
    all.retain(|(p, _)| *p != path && p.exists());
    all.insert(0, (path, SystemTime::now()));
    all.truncate(KEEP);
    let text: String = all
        .iter()
        .filter(|(p, _)| !p.to_string_lossy().contains(['\n', '\t']))
        .map(|(p, at)| format!("{}\t{}\n", at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()), p.display()))
        .collect();
    let _ = std::fs::create_dir_all(home);
    let _ = std::fs::write(file(home), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_the_newest_first_once_each_and_forgets_what_is_gone() {
        let root = std::env::temp_dir().join(format!("omanote-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        let (a, b) = (root.join("cfg/hyprland.lua"), root.join("cfg/bindings.lua"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        assert!(list(&home).is_empty(), "nothing yet, and no file is no problem");
        opened(&home, &a);
        opened(&home, &b);
        opened(&home, &a);
        let paths: Vec<PathBuf> = list(&home).into_iter().map(|(p, _)| p).collect();
        assert_eq!(paths, [a.canonicalize().unwrap(), b.canonicalize().unwrap()]);
        std::fs::remove_file(&b).unwrap();
        assert_eq!(list(&home).len(), 1, "a deleted file is not offered");
        let _ = std::fs::remove_dir_all(&root);
    }
}
