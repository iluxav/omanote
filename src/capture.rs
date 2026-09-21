//! `omanote --capture "text"`: jot a line into the inbox note without opening
//! the editor. This is what the Omarchy quick-capture overlay calls.
//!
//! Lines collect under a heading per day:
//!
//!     ## 2026-09-19
//!     - 14:02 call the dentist
//!     - 16:40 idea: sync indicator in the bar

use std::path::{Path, PathBuf};

use crate::remind;

pub fn inbox(vault: &Path) -> PathBuf {
    vault.join("inbox.md")
}

/// The note with one more line in it.
pub fn appended(existing: &str, date: &str, time: &str, text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = existing.trim_end().to_string();
    let heading = format!("## {date}");
    // Today's heading is only reused while it is still the last one.
    let today_is_last = out.lines().rev().find(|l| l.starts_with("## ")).is_some_and(|l| l.trim() == heading);
    if out.is_empty() {
        out = "# Inbox".to_string();
    }
    if !today_is_last {
        out.push_str(&format!("\n\n{heading}"));
    }
    out.push_str(&format!("\n- {time} {text}\n"));
    out
}

fn now() -> (String, String) {
    let out = std::process::Command::new("date").arg("+%Y-%m-%d %H:%M").output().ok();
    let stamp = out.and_then(|o| String::from_utf8(o.stdout).ok()).unwrap_or_default();
    let (date, time) = stamp.trim().split_once(' ').unwrap_or(("", ""));
    (date.to_string(), time.to_string())
}

/// `timer`: make sure the reminder timer runs if this capture needs it.
pub fn capture(vault: &Path, text: &str, timer: bool) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("nothing to capture — usage: omanote --capture \"some text\"".into());
    }
    let file = inbox(vault);
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let (date, time) = now();
    // "call the dentist !tomorrow 9:00": the time goes into the note as a
    // date, which is what the reminder timer reads.
    let (words, reminder) = remind::split(text.trim(), remind::now());
    let line = match &reminder {
        Ok(Some(when)) => format!("{words} ⏰ {}", remind::render(when)).trim().to_string(),
        _ => words,
    };
    std::fs::create_dir_all(vault).and_then(|_| std::fs::write(&file, appended(&existing, &date, &time, &line))).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    let mut said = format!("Captured to {}", crate::vaults::tilde(&file));
    match reminder {
        Ok(Some(when)) => {
            let at = remind::next(&when, remind::now()).map(remind::friendly).unwrap_or_default();
            let how = if matches!(when, remind::When::Every(_)) { format!("{}, next {at}", remind::render(&when)) } else { at };
            said.push_str(&format!(" · reminder {how}"));
            // A reminder nobody is watching for is a promise broken: see to the timer.
            if timer && !remind::timer_on() {
                match remind::turn_on() {
                    Ok(_) => said.push_str(" · reminder timer turned on (omanote --reminders off stops it)"),
                    Err(e) => said.push_str(&format!(" · BUT the reminder timer is not running: {e}")),
                }
            }
        }
        Err(what) => said.push_str(&format!(" · no reminder: \"{what}\" is not a time I can read (try !30m, !fri 10:00, !every mon 3pm)")),
        Ok(None) => {}
    }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_collect_under_a_heading_per_day() {
        let day1 = appended("", "2026-09-19", "14:02", "  call   the dentist ");
        assert_eq!(day1, "# Inbox\n\n## 2026-09-19\n- 14:02 call the dentist\n");
        let again = appended(&day1, "2026-09-19", "16:40", "an idea");
        assert_eq!(again, "# Inbox\n\n## 2026-09-19\n- 14:02 call the dentist\n- 16:40 an idea\n");
        let next = appended(&again, "2026-09-20", "09:00", "new day");
        assert!(next.ends_with("- 16:40 an idea\n\n## 2026-09-20\n- 09:00 new day\n"), "{next}");
        // Hand-written notes in the inbox are left alone.
        let mine = appended("my own notes\n", "2026-09-20", "09:00", "x");
        assert_eq!(mine, "my own notes\n\n## 2026-09-20\n- 09:00 x\n");
    }

    #[test]
    fn writes_the_inbox_and_refuses_nothing() {
        let vault = std::env::temp_dir().join(format!("omanote-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&vault);
        assert!(capture(&vault, "   ", false).is_err());
        assert!(capture(&vault, "first thing", false).unwrap().contains("inbox.md"));
        capture(&vault, "second thing", false).unwrap();
        let text = std::fs::read_to_string(inbox(&vault)).unwrap();
        assert_eq!(text.matches("## ").count(), 1);
        assert!(text.contains("first thing") && text.contains("second thing"));

        let said = capture(&vault, "pick up liam !every mon 3pm, tue 1pm", false).unwrap();
        assert!(said.contains("reminder every mon 15:00, tue 13:00, next "), "{said}");
        let said = capture(&vault, "so !important", false).unwrap();
        assert!(said.contains("no reminder") && said.contains("!important"), "{said}");
        let text = std::fs::read_to_string(inbox(&vault)).unwrap();
        assert!(text.contains(" pick up liam ⏰ every mon 15:00, tue 13:00\n") && text.contains(" so !important\n"), "{text}");
        let _ = std::fs::remove_dir_all(vault);
    }
}
