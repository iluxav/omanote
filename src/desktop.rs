//! `omanote --omarchy`: make omanote part of the desktop. Registers an app
//! launcher entry (through Omarchy's own `omarchy-tui-install` when it is
//! there, a plain .desktop file otherwise) and adds entries to the Omarchy
//! menu. Everything it does is printed, and nothing that exists is replaced.

use std::path::{Path, PathBuf};
use std::process::Command;

const ICON_URL: &str = "https://raw.githubusercontent.com/iluxav/omanote/main/assets/icon-256.png";

const MENU: &str = r#"  "notes": {"icon":"󰎞","label":"Notes","aliases":["omanote"]},
  "notes.open": {"icon":"󰎞","label":"Open omanote","action":"omarchy-launch-or-focus-tui omanote"},
  "notes.capture": {"icon":"󰏫","label":"Quick note","action":"omarchy-shell shell toggle iluxav.omanote","when":"test -d ~/.config/omarchy/plugins/iluxav.omanote"},
  "notes.sync": {"icon":"󰓦","label":"Sync GitHub vaults","action":"omarchy-launch-floating-terminal-with-presentation omanote --sync"}"#;

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn on_path(program: &str) -> bool {
    Command::new("sh").args(["-c", &format!("command -v {program}")]).output().is_ok_and(|o| o.status.success())
}

/// JSONC without its comments, to tell an untouched template from a file with entries in it.
fn without_comments(text: &str) -> String {
    text.lines().map(|l| l.trim()).filter(|l| !l.starts_with("//")).collect::<Vec<_>>().join("")
}

/// The menu file with our entries added, if that can be done without guessing:
/// only into a file that has no entries of its own yet.
fn menu_with_entries(text: &str) -> Option<String> {
    if without_comments(text) != "{}" {
        return None;
    }
    let close = text.rfind('}')?;
    Some(format!("{}{MENU}\n{}", &text[..close], &text[close..]))
}

fn launcher(apps: &Path, out: &mut Vec<String>) {
    let entry = apps.join("Omanote.desktop");
    if entry.exists() || apps.join("omanote.desktop").exists() {
        out.push(format!("✓ App launcher entry already there: {}", crate::vaults::tilde(&entry)));
        return;
    }
    if on_path("omarchy-tui-install") {
        let ok = Command::new("omarchy-tui-install").args(["Omanote", "omanote", "float", ICON_URL]).status().is_ok_and(|s| s.success());
        out.push(if ok { "✓ Added Omanote to the app launcher (omarchy-tui-install)".into() } else { "✗ omarchy-tui-install failed; run it by hand: omarchy-tui-install".into() });
        return;
    }
    let body = "[Desktop Entry]\nVersion=1.0\nType=Application\nName=Omanote\nComment=Markdown notes in the terminal\nExec=omanote\nTerminal=true\nIcon=accessories-text-editor\nCategories=Utility;TextEditor;\n";
    let ok = std::fs::create_dir_all(apps).and_then(|_| std::fs::write(&entry, body)).is_ok();
    out.push(if ok { format!("✓ Added {}", crate::vaults::tilde(&entry)) } else { format!("✗ Could not write {}", entry.display()) });
}

fn menu(file: &Path, out: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        return; // not an Omarchy desktop with a menu to extend
    };
    if text.contains("omanote") {
        out.push("✓ Omarchy menu already has the Notes entries".into());
        return;
    }
    match menu_with_entries(&text) {
        Some(updated) if std::fs::write(file, &updated).is_ok() => out.push(format!("✓ Added Notes to the Omarchy menu ({})", crate::vaults::tilde(file))),
        _ => out.push(format!("• Your Omarchy menu file has entries of its own, so add these yourself to {}:\n\n{MENU}\n", crate::vaults::tilde(file))),
    }
}

pub fn integrate() -> String {
    let mut out = Vec::new();
    launcher(&home().join(".local/share/applications"), &mut out);
    menu(&home().join(".config/omarchy/extensions/omarchy-menu.jsonc"), &mut out);
    if on_path("omarchy-shell") {
        out.push(
            "• Bar icon and quick capture come from the plugin:\n    omarchy plugin add https://github.com/iluxav/omarchy-omanote --enable\n  and, for a quick-capture key, in ~/.config/hypr/bindings.lua:\n    o.bind(\"SUPER + CTRL + N\", \"Quick note\", \"omarchy-shell shell toggle iluxav.omanote\")"
                .into(),
        );
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_entries_go_into_an_untouched_template_only() {
        let template = "{\n  // Extend the menu.\n  // \"personal\": {\"icon\":\"x\"},\n}\n";
        let updated = menu_with_entries(template).unwrap();
        assert!(updated.starts_with("{\n  // Extend the menu.") && updated.trim_end().ends_with('}'));
        assert!(updated.contains("\"notes.sync\"") && !updated.contains("},\n}"), "no trailing comma before the brace");
        // The result is valid JSON once the comments are gone.
        let json: String = updated.lines().filter(|l| !l.trim().starts_with("//")).collect();
        assert_eq!(json.matches('{').count(), json.matches('}').count());

        assert_eq!(menu_with_entries("{\n  \"mine\": {\"label\":\"Mine\"}\n}\n"), None, "never edits a file with your entries");
        assert_eq!(menu_with_entries("not json"), None);
    }

    #[test]
    fn writes_a_plain_launcher_and_leaves_an_existing_one() {
        let apps = std::env::temp_dir().join(format!("omanote-apps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&apps);
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(apps.join("Omanote.desktop"), "mine").unwrap();
        let mut out = Vec::new();
        launcher(&apps, &mut out);
        assert!(out[0].contains("already there"));
        assert_eq!(std::fs::read_to_string(apps.join("Omanote.desktop")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(apps);
    }
}
