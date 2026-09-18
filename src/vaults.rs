//! Vaults: the folders notes are searched in.
//!
//! The default vault (`~/.omanote/docs`) always comes first and is where new
//! notes are created. More are registered in `~/.omanote/origins.toml`, either
//! a local folder (`--vl`) or a GitHub repo cloned under `~/.omanote/vaults`
//! (`--vlgh`).

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vault {
    pub path: PathBuf,
    /// `owner/repo` when the vault is a clone we made.
    pub github: Option<String>,
}

impl Vault {
    /// Short name shown in front of this vault's notes in the picker.
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

/// `$OMANOTE_HOME`, else `~/.omanote`.
pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("OMANOTE_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".omanote")
}

fn default_vault(home: &Path) -> Vault {
    let path = std::env::var_os("OMANOTE_VAULT").filter(|v| !v.is_empty()).map_or_else(|| home.join("docs"), PathBuf::from);
    Vault { path, github: None }
}

fn config(home: &Path) -> PathBuf {
    home.join("origins.toml")
}

/// The default vault followed by every registered one.
pub fn all(home: &Path) -> Vec<Vault> {
    let mut vaults = vec![default_vault(home)];
    for vault in registered(home) {
        if !vaults.iter().any(|v| v.path == vault.path) {
            vaults.push(vault);
        }
    }
    vaults
}

/// The vault a note lives in (for resolving its images), else the default one.
pub fn root_of(note: Option<&Path>, vaults: &[Vault]) -> PathBuf {
    let inside = note.and_then(|n| vaults.iter().filter(|v| n.starts_with(&v.path)).max_by_key(|v| v.path.as_os_str().len()));
    inside.or(vaults.first()).map(|v| v.path.clone()).unwrap_or_default()
}

// ---- origins.toml --------------------------------------------------------
// Only the little TOML we write ourselves: `[[vault]]` tables of string keys.

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote(value: &str) -> Option<String> {
    let inner = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(out)
}

fn registered(home: &Path) -> Vec<Vault> {
    let text = std::fs::read_to_string(config(home)).unwrap_or_default();
    let mut vaults: Vec<Vault> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line == "[[vault]]" {
            vaults.push(Vault { path: PathBuf::new(), github: None });
        } else if let (Some(vault), Some((key, value))) = (vaults.last_mut(), line.split_once('=')) {
            match (key.trim(), unquote(value)) {
                ("path", Some(path)) => vault.path = PathBuf::from(path),
                ("github", Some(repo)) => vault.github = Some(repo),
                _ => {}
            }
        }
    }
    vaults.retain(|v| !v.path.as_os_str().is_empty());
    vaults
}

fn write(home: &Path, vaults: &[Vault]) -> Result<(), String> {
    let mut text = String::from("# Extra places omanote looks for notes. Managed by `omanote --vl / --vlgh / --vlrm`.\n");
    for vault in vaults {
        text.push_str(&format!("\n[[vault]]\npath = {}\n", quote(&vault.path.to_string_lossy())));
        if let Some(repo) = &vault.github {
            text.push_str(&format!("github = {}\n", quote(repo)));
        }
    }
    std::fs::create_dir_all(home).and_then(|_| std::fs::write(config(home), text)).map_err(|e| format!("cannot write {}: {e}", config(home).display()))
}

fn register(home: &Path, vault: Vault) -> Result<bool, String> {
    let mut vaults = registered(home);
    if vaults.iter().any(|v| v.path == vault.path) || default_vault(home).path == vault.path {
        return Ok(false);
    }
    vaults.push(vault);
    write(home, &vaults).map(|_| true)
}

// ---- commands ------------------------------------------------------------

pub fn tilde(path: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.is_empty() => format!("~/{}", rest.display()).trim_end_matches('/').to_string(),
        _ => path.display().to_string(),
    }
}

/// `--vl <folder>`
pub fn add_local(home: &Path, folder: &str) -> Result<String, String> {
    let expanded = match folder.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(folder),
    };
    let path = expanded.canonicalize().map_err(|_| format!("{folder} does not exist — create the folder first"))?;
    if !path.is_dir() {
        return Err(format!("{folder} is a file, not a folder"));
    }
    let fresh = register(home, Vault { path: path.clone(), github: None })?;
    Ok(format!("{} {}", if fresh { "Added vault" } else { "Already a vault:" }, tilde(&path)))
}

/// `owner/repo` out of a slug, an https URL or an ssh URL.
pub fn github_slug(spec: &str) -> Option<String> {
    let spec = spec.trim().trim_end_matches('/');
    let rest = ["https://github.com/", "http://github.com/", "git@github.com:", "github.com/"]
        .iter()
        .find_map(|p| spec.strip_prefix(p))
        .unwrap_or(spec);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    let ok = |part: &str| {
        part.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

fn git(args: &[&str], quiet_prompts: bool) -> bool {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if quiet_prompts {
        // Fail instead of asking for a username when https needs credentials.
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    cmd.status().is_ok_and(|s| s.success())
}

/// Clone `sources` (tried in order) into the vault folder for `slug`, or update
/// the clone that is already there, and register it.
fn add_clone(home: &Path, slug: &str, sources: &[String]) -> Result<String, String> {
    let dest = home.join("vaults").join(slug);
    let dest_str = dest.to_string_lossy().into_owned();
    if dest.join(".git").exists() {
        println!("{} is already cloned, pulling the latest…", tilde(&dest));
        if !git(&["-C", &dest_str, "pull", "--ff-only"], false) {
            println!("(could not pull; keeping what is there)");
        }
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let cloned = sources.iter().enumerate().any(|(i, source)| {
            println!("Cloning {source} …");
            git(&["clone", "--", source, &dest_str], i + 1 < sources.len())
        });
        if !cloned {
            return Err(format!("could not clone {slug} — check the name and that you have access"));
        }
    }
    let fresh = register(home, Vault { path: dest.clone(), github: Some(slug.to_string()) })?;
    Ok(format!("{} {slug} → {}", if fresh { "Added vault" } else { "Vault is up to date:" }, tilde(&dest)))
}

/// `--vlgh <owner/repo>`
pub fn add_github(home: &Path, spec: &str) -> Result<String, String> {
    let slug = github_slug(spec).ok_or_else(|| format!("\"{spec}\" is not a GitHub repo — expected owner/repo"))?;
    // Public repos clone over https; private ones usually need the ssh key.
    add_clone(home, &slug, &[format!("https://github.com/{slug}.git"), format!("git@github.com:{slug}.git")])
}

/// `--vlrm <folder | owner/repo | name>`: forget a vault. Its files stay where they are.
pub fn remove(home: &Path, spec: &str) -> Result<String, String> {
    let mut vaults = registered(home);
    let as_path = PathBuf::from(spec).canonicalize().ok();
    let slug = github_slug(spec);
    let matches = |v: &Vault| Some(&v.path) == as_path.as_ref() || (slug.is_some() && v.github == slug) || v.name() == spec;
    let hits: Vec<usize> = (0..vaults.len()).filter(|&i| matches(&vaults[i])).collect();
    match hits[..] {
        [] => Err(format!("no vault matches \"{spec}\" — see `omanote --vls`")),
        [i] => {
            let gone = vaults.remove(i);
            write(home, &vaults)?;
            Ok(format!("Removed vault {} (the files are still there)", tilde(&gone.path)))
        }
        _ => Err(format!("\"{spec}\" matches more than one vault — use the full path")),
    }
}

/// `--vls`
pub fn list(home: &Path) -> String {
    let vaults = all(home);
    let width = vaults.iter().map(|v| tilde(&v.path).chars().count()).max().unwrap_or(0);
    let mut out = String::from("Vaults — Ctrl+P searches all of them, new notes go in the first:\n");
    for (i, vault) in vaults.iter().enumerate() {
        let notes = crate::picker::Picker::open(std::slice::from_ref(vault)).total();
        let kind = match (&vault.github, i) {
            (Some(repo), _) => format!("github: {repo}"),
            (None, 0) => "default".to_string(),
            (None, _) => String::new(),
        };
        let missing = if vault.path.is_dir() { "" } else { "  (folder missing)" };
        let notes = format!("{notes} {}", if notes == 1 { "note " } else { "notes" });
        out.push_str(&format!("  {:<width$}  {notes:>11}  {kind}{missing}\n", tilde(&vault.path)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omanote-vaults-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn adds_lists_and_removes_local_vaults() {
        let home = scratch("local");
        let notes = home.join("my \"odd\" notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("a.md"), "x").unwrap();

        assert!(add_local(&home, notes.to_str().unwrap()).unwrap().starts_with("Added vault"));
        assert!(add_local(&home, notes.to_str().unwrap()).unwrap().starts_with("Already"));
        assert!(add_local(&home, "/no/such/folder").unwrap_err().contains("does not exist"));
        assert!(add_local(&home, notes.join("a.md").to_str().unwrap()).unwrap_err().contains("not a folder"));

        let vaults = all(&home);
        assert_eq!(vaults.len(), 2);
        assert_eq!(vaults[1], Vault { path: notes.clone(), github: None }, "quotes in the path survive the file");
        assert!(list(&home).contains("1 note "));

        assert!(remove(&home, "nope").is_err());
        assert!(remove(&home, "my \"odd\" notes").unwrap().contains("still there"));
        assert_eq!(all(&home).len(), 1);
        assert!(notes.join("a.md").exists());
    }

    #[test]
    fn reads_github_names_in_every_form() {
        for spec in ["iluxav/my-notes", "https://github.com/iluxav/my-notes", "https://github.com/iluxav/my-notes.git/", "git@github.com:iluxav/my-notes.git"] {
            assert_eq!(github_slug(spec).as_deref(), Some("iluxav/my-notes"), "{spec}");
        }
        for bad in ["just-a-name", "-x/repo", "owner/--upload-pack=evil", "a/b/c", "owner/re po", ""] {
            assert_eq!(github_slug(bad), None, "{bad}");
        }
    }

    #[test]
    fn clones_registers_and_updates() {
        let home = scratch("clone");
        let origin = home.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::write(origin.join("hello.md"), "# hi\n").unwrap();
        let o = origin.to_str().unwrap();
        assert!(git(&["-C", o, "init", "-q"], true));
        assert!(git(&["-C", o, "add", "."], true));
        assert!(git(&["-C", o, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"], true));

        // The first source fails, the second works: that is the https → ssh fallback.
        let sources = [home.join("missing").display().to_string(), o.to_string()];
        let msg = add_clone(&home, "someone/notes", &sources).unwrap();
        assert!(msg.starts_with("Added vault someone/notes"), "{msg}");
        let dest = home.join("vaults/someone/notes");
        assert!(dest.join("hello.md").exists());
        assert_eq!(all(&home)[1], Vault { path: dest.clone(), github: Some("someone/notes".into()) });

        // Again: pulls, does not duplicate.
        assert!(add_clone(&home, "someone/notes", &sources).unwrap().starts_with("Vault is up to date"));
        assert_eq!(all(&home).len(), 2);

        assert!(remove(&home, "someone/notes").is_ok());
        assert!(dest.join("hello.md").exists(), "removing never deletes files");
        assert!(add_clone(&home, "someone/else", &[home.join("missing").display().to_string()]).is_err());
    }

    #[test]
    fn finds_the_vault_a_note_belongs_to() {
        let vaults = vec![
            Vault { path: "/v/docs".into(), github: None },
            Vault { path: "/v/work".into(), github: None },
            Vault { path: "/v/work/deep".into(), github: None },
        ];
        assert_eq!(root_of(Some(Path::new("/v/work/deep/a.md")), &vaults), PathBuf::from("/v/work/deep"));
        assert_eq!(root_of(Some(Path::new("/elsewhere/a.md")), &vaults), PathBuf::from("/v/docs"));
        assert_eq!(root_of(None, &vaults), PathBuf::from("/v/docs"));
    }
}
