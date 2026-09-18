//! Keeps GitHub vaults (`--vlgh`) in step with their remote, without ever
//! making the editor wait.
//!
//! Every git operation runs as a detached `sh` process in a session of its
//! own. Opening a note shows what is on disk and pulls in the background (the editor reloads the
//! note if it changed). Saving marks the vault as having work to send; that is
//! committed and pushed once the note has been quiet for a while, and when you
//! switch notes or quit — and because the process is detached, quitting does
//! not wait for the network.
//!
//! Only vaults cloned with `--vlgh` take part. A plain `--vl` folder is never
//! committed to, even if it happens to be a git repository.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::vaults::Vault;

/// Commit and push this long after the last save in a vault.
const QUIET: Duration = Duration::from_secs(30);
/// Do not pull the same vault more often than this.
const PULL_EVERY: Duration = Duration::from_secs(60);

/// $1 vault, $2 status file, $3 commit message (empty = only pull).
///
/// One sync per vault at a time: later ones wait for the lock, so a save made
/// while a push is running still gets its own turn.
const SCRIPT: &str = r#"
cd "$1" || exit 1
status="$2"; msg="$3"; log="$status.log"
lock=.git/omanote-sync.lock
# The status is the very last thing written: whoever reads it can rely on the
# lock being free and git being done.
finish() { rmdir "$lock" 2>/dev/null; printf '%s\n' "$1" > "$status.tmp" && mv -f "$status.tmp" "$status"; exit 0; }
tries=0
until mkdir "$lock" 2>/dev/null; do
    tries=$((tries + 1))
    [ "$tries" -gt 90 ] && rm -rf "$lock"
    sleep 1
done
trap 'rmdir "$lock" 2>/dev/null' EXIT
: > "$log"
export GIT_TERMINAL_PROMPT=0

if [ -n "$msg" ]; then
    git add -A >>"$log" 2>&1 || finish "error could not stage changes"
    if ! git diff --cached --quiet; then
        git commit -q -m "$msg" >>"$log" 2>&1 || finish "error could not commit — is git user.name / user.email set?"
    fi
fi
if ! git pull --rebase --autostash -q >>"$log" 2>&1; then
    git rebase --abort >>"$log" 2>&1
    [ -n "$msg" ] && finish "error conflict with GitHub — your changes are committed locally; resolve with git in $1"
    finish "error could not pull (offline?)"
fi
# A pull also sends commits an earlier, offline push left behind.
if [ -z "$msg" ] && [ -z "$(git log '@{u}..' --oneline 2>/dev/null)" ]; then
    finish "ok pulled"
fi
git push -q >>"$log" 2>&1 || finish "error could not push (offline?) — will retry after the next save"
finish "ok pushed"
"#;

fn run(vault: &Path, status: &Path, message: &str) -> std::io::Result<Child> {
    if let Some(dir) = status.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut cmd = Command::new("sh");
    cmd.args(["-c", SCRIPT, "omanote-sync"]).arg(vault).arg(status).arg(message);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // We usually quit a moment after starting a push, and the terminal often
    // closes with us. A new session has no terminal to be hung up by. It has
    // to happen before exec: `nohup` would lose the race.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    cmd.spawn()
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[derive(Default)]
struct State {
    /// Notes saved since the last push, and when the latest save was.
    unsent: Vec<String>,
    last_save: Option<Instant>,
    last_pull: Option<Instant>,
    running: Vec<Child>,
    seen_status: Option<SystemTime>,
}

pub struct Sync {
    home: PathBuf,
    enabled: bool,
    vaults: HashMap<PathBuf, State>,
}

impl Sync {
    pub fn new(home: PathBuf) -> Self {
        let enabled = !std::env::var("OMANOTE_SYNC").is_ok_and(|v| v == "off");
        Sync { home, enabled, vaults: HashMap::new() }
    }

    fn status_file(&self, vault: &Path) -> PathBuf {
        self.home.join("sync").join(format!("{:016x}", fnv1a(&vault.to_string_lossy())))
    }

    /// The GitHub vault a note lives in, if any.
    fn vault_of<'a>(&self, note: &Path, vaults: &'a [Vault]) -> Option<&'a Vault> {
        if !self.enabled {
            return None;
        }
        vaults.iter().filter(|v| v.github.is_some() && note.starts_with(&v.path)).max_by_key(|v| v.path.as_os_str().len())
    }

    fn start(&mut self, vault: &Path, message: &str) {
        let status = self.status_file(vault);
        let state = self.vaults.entry(vault.to_path_buf()).or_default();
        // Whatever the status file says now is old news.
        state.seen_status = std::fs::metadata(&status).and_then(|m| m.modified()).ok();
        if let Ok(child) = run(vault, &status, message) {
            state.running.push(child);
        }
    }

    /// A note is being opened: bring its vault up to date, in the background.
    pub fn opened(&mut self, note: &Path, vaults: &[Vault]) {
        let Some(vault) = self.vault_of(note, vaults).map(|v| v.path.clone()) else { return };
        self.pull(&vault);
    }

    /// The picker is coming up: refresh every GitHub vault so the list is current next time.
    pub fn pull_all(&mut self, vaults: &[Vault]) {
        if !self.enabled {
            return;
        }
        for vault in vaults.iter().filter(|v| v.github.is_some()) {
            self.pull(&vault.path.clone());
        }
    }

    fn pull(&mut self, vault: &Path) {
        let state = self.vaults.entry(vault.to_path_buf()).or_default();
        let recent = state.last_pull.is_some_and(|at| at.elapsed() < PULL_EVERY);
        // Work waiting to be sent pulls as part of its own push.
        if recent || !state.unsent.is_empty() {
            return;
        }
        state.last_pull = Some(Instant::now());
        self.start(vault, "");
    }

    /// A note was written to disk.
    pub fn saved(&mut self, note: &Path, vaults: &[Vault]) {
        let Some(vault) = self.vault_of(note, vaults) else { return };
        let name = note.strip_prefix(&vault.path).unwrap_or(note).to_string_lossy().into_owned();
        let state = self.vaults.entry(vault.path.clone()).or_default();
        if !state.unsent.contains(&name) {
            state.unsent.push(name);
        }
        state.last_save = Some(Instant::now());
    }

    fn push(&mut self, vault: &Path) {
        let Some(state) = self.vaults.get_mut(vault) else { return };
        let names = std::mem::take(&mut state.unsent);
        if names.is_empty() {
            return;
        }
        state.last_save = None;
        state.last_pull = Some(Instant::now());
        let message = match &names[..] {
            [one] => format!("omanote: update {one}"),
            many => format!("omanote: update {} notes\n\n{}", many.len(), many.join("\n")),
        };
        self.start(vault, &message);
    }

    /// Send everything that is waiting, now (switching notes, quitting). Does not block.
    pub fn flush(&mut self) {
        let waiting: Vec<PathBuf> = self.vaults.iter().filter(|(_, s)| !s.unsent.is_empty()).map(|(v, _)| v.clone()).collect();
        for vault in waiting {
            self.push(&vault);
        }
    }

    /// Call regularly. Pushes vaults that have gone quiet, and returns a message
    /// when a background sync has something to say.
    pub fn tick(&mut self) -> Option<String> {
        self.tick_after(QUIET)
    }

    fn tick_after(&mut self, quiet: Duration) -> Option<String> {
        let due: Vec<PathBuf> =
            self.vaults.iter().filter(|(_, s)| s.last_save.is_some_and(|at| at.elapsed() >= quiet)).map(|(v, _)| v.clone()).collect();
        for vault in due {
            self.push(&vault);
        }

        let mut news = None;
        let files: Vec<(PathBuf, PathBuf)> = self.vaults.keys().map(|v| (v.clone(), self.status_file(v))).collect();
        for (vault, file) in files {
            let state = self.vaults.get_mut(&vault)?;
            state.running.retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
            let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
            if modified.is_none() || modified == state.seen_status {
                continue;
            }
            state.seen_status = modified;
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            match text.trim().split_once(' ') {
                Some(("ok", "pushed")) => news = Some("Synced to GitHub".to_string()),
                Some(("error", why)) => news = Some(format!("GitHub sync: {why}")),
                _ => {}
            }
        }
        news
    }

    /// For the status line: what is going on with this note's vault.
    pub fn state(&self, note: &Path, vaults: &[Vault]) -> Option<&'static str> {
        let vault = self.vault_of(note, vaults)?;
        let state = self.vaults.get(&vault.path);
        Some(match state {
            Some(s) if !s.running.is_empty() => "syncing…",
            Some(s) if !s.unsent.is_empty() => "to sync",
            _ => "github",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare "GitHub" with two clones of it, as two machines would have.
    fn world(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("omanote-sync-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        git(&root, &["init", "-q", "--bare", "-b", "main", "hub.git"]);
        for clone in ["here", "there"] {
            git(&root, &["clone", "-q", "hub.git", clone]);
            git(&root.join(clone), &["config", "user.name", clone]);
            git(&root.join(clone), &["config", "user.email", "t@t"]);
            git(&root.join(clone), &["checkout", "-q", "-B", "main"]);
        }
        let here = root.join("here");
        std::fs::write(here.join("first.md"), "one\n").unwrap();
        git(&here, &["add", "-A"]);
        git(&here, &["commit", "-qm", "init"]);
        git(&here, &["push", "-q", "-u", "origin", "main"]);
        git(&root.join("there"), &["pull", "-q", "origin", "main"]);
        git(&root.join("there"), &["branch", "-q", "--set-upstream-to=origin/main"]);
        (root.clone(), here, root.join("there"))
    }

    fn vault(path: &Path) -> Vec<Vault> {
        vec![Vault { path: "/nowhere/docs".into(), github: None }, Vault { path: path.to_path_buf(), github: Some("me/notes".into()) }]
    }

    fn wait(sync: &mut Sync) -> String {
        for _ in 0..400 {
            if let Some(msg) = sync.tick_after(Duration::ZERO) {
                return msg;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("sync never reported back");
    }

    fn settle(sync: &mut Sync, vault: &Path) {
        for _ in 0..400 {
            sync.tick_after(QUIET);
            if sync.vaults.get(vault).is_none_or(|s| s.running.is_empty()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("sync never finished");
    }

    #[test]
    fn saves_are_committed_and_pushed_and_pulled_elsewhere() {
        let (root, here, there) = world("roundtrip");
        let mut sync = Sync::new(root.join("home"));
        sync.enabled = true;

        std::fs::write(here.join("idea.md"), "# Idea\n").unwrap();
        sync.saved(&here.join("idea.md"), &vault(&here));
        sync.saved(&here.join("idea.md"), &vault(&here));
        assert_eq!(sync.state(&here.join("idea.md"), &vault(&here)), Some("to sync"));
        assert_eq!(sync.tick_after(QUIET), None, "not quiet for long enough yet");
        assert_eq!(wait(&mut sync), "Synced to GitHub");
        assert_eq!(git(&here, &["log", "-1", "--format=%s"]), "omanote: update idea.md");
        assert_eq!(git(&root.join("hub.git"), &["log", "-1", "--format=%s", "main"]), "omanote: update idea.md");
        // The status arrives a hair before the process is gone.
        settle(&mut sync, &here);
        assert_eq!(sync.state(&here.join("idea.md"), &vault(&here)), Some("github"));

        // The other machine opens a note: the pull brings the new one in.
        let mut other = Sync::new(root.join("home2"));
        other.enabled = true;
        other.opened(&there.join("first.md"), &vault(&there));
        settle(&mut other, &there);
        assert_eq!(std::fs::read_to_string(there.join("idea.md")).unwrap(), "# Idea\n");

        // Pulls are throttled; notes outside GitHub vaults are left alone.
        other.opened(&there.join("first.md"), &vault(&there));
        assert!(other.vaults[&there].running.is_empty());
        sync.saved(Path::new("/nowhere/docs/x.md"), &vault(&here));
        assert!(!sync.vaults.contains_key(Path::new("/nowhere/docs")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_conflict_is_reported_and_leaves_the_repo_usable() {
        let (root, here, there) = world("conflict");
        std::fs::write(there.join("first.md"), "theirs\n").unwrap();
        git(&there, &["commit", "-qam", "theirs"]);
        git(&there, &["push", "-q"]);

        let mut sync = Sync::new(root.join("home"));
        sync.enabled = true;
        std::fs::write(here.join("first.md"), "mine\n").unwrap();
        sync.saved(&here.join("first.md"), &vault(&here));
        sync.flush();
        let msg = wait(&mut sync);
        assert!(msg.contains("conflict with GitHub"), "{msg}");
        assert_eq!(std::fs::read_to_string(here.join("first.md")).unwrap(), "mine\n", "your text is untouched");
        assert_eq!(git(&here, &["log", "-1", "--format=%s"]), "omanote: update first.md", "and committed locally");
        assert!(!here.join(".git/rebase-merge").exists() && !here.join(".git/rebase-apply").exists(), "no rebase left half done");
        assert!(!here.join(".git/omanote-sync.lock").exists(), "lock released");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn opening_a_note_sends_commits_left_behind_offline() {
        let (root, here, _) = world("leftover");
        std::fs::write(here.join("offline.md"), "written on a plane\n").unwrap();
        git(&here, &["add", "-A"]);
        git(&here, &["commit", "-qm", "omanote: update offline.md"]);

        let mut sync = Sync::new(root.join("home"));
        sync.enabled = true;
        sync.opened(&here.join("first.md"), &vault(&here));
        assert_eq!(wait(&mut sync), "Synced to GitHub");
        assert_eq!(git(&root.join("hub.git"), &["log", "-1", "--format=%s", "main"]), "omanote: update offline.md");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn can_be_switched_off() {
        let mut sync = Sync::new(PathBuf::from("/tmp/omanote-unused"));
        sync.enabled = false;
        let vaults = vault(Path::new("/v/gh"));
        sync.saved(Path::new("/v/gh/a.md"), &vaults);
        sync.opened(Path::new("/v/gh/a.md"), &vaults);
        sync.pull_all(&vaults);
        assert!(sync.vaults.is_empty());
        assert_eq!(sync.state(Path::new("/v/gh/a.md"), &vaults), None);
    }
}
