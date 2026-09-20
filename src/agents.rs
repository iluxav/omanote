//! Which AI agents Ctrl+G can open, and the little menu that asks.
//!
//! Nothing is hardcoded to one product: omanote knows how to start a handful
//! of agent CLIs, offers the ones that are actually installed, and takes your
//! own from the settings (`agent.<name> = "<command>"`).

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    /// What to run. `{context}`, `{file}` and `{dir}` are filled in.
    pub command: String,
}

/// Agents omanote can start without being told how: (name, program, command).
/// Where the CLI has a way to take an opening prompt, the context goes in that
/// way. Only Claude Code can take it silently, as an addition to its system
/// prompt; for the others it is their first message. The rest are started
/// plain, in the right folder, because their flags are not something to guess.
const KNOWN: [(&str, &str, &str); 11] = [
    ("Claude Code", "claude", "claude --append-system-prompt {context}"),
    ("Codex", "codex", "codex {context}"),
    ("Gemini", "gemini", "gemini -i {context}"),
    ("opencode", "opencode", "opencode --prompt {context}"),
    ("Qwen Code", "qwen", "qwen -i {context}"),
    ("Aider", "aider", "aider {file}"),
    ("Cursor Agent", "cursor-agent", "cursor-agent"),
    ("GitHub Copilot", "copilot", "copilot"),
    ("Crush", "crush", "crush"),
    ("Amp", "amp", "amp"),
    ("Goose", "goose", "goose session"),
];

fn program(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

/// Which of `programs` exist, asked of the user's login shell in one go: the
/// same shell, and so the same PATH, the agent will be started with.
fn installed(programs: &[&str]) -> Vec<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    let script = r#"for p in "$@"; do command -v "$p" >/dev/null 2>&1 && printf '%s\n' "$p"; done"#;
    let out = Command::new(shell).args(["-lc", script, "omanote-agents"]).args(programs).stdin(Stdio::null()).stderr(Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect()).unwrap_or_default()
}

/// Your own agents first (in the order of the settings file), then the known
/// ones that are installed. One of yours replaces a known one of the same name.
pub fn available(custom: &[(String, String)]) -> Vec<Agent> {
    let mut programs: Vec<&str> = custom.iter().map(|(_, c)| program(c)).collect();
    programs.extend(KNOWN.iter().map(|(_, p, _)| *p));
    let found = installed(&programs);
    pick(custom, &found)
}

fn pick(custom: &[(String, String)], found: &[String]) -> Vec<Agent> {
    let has = |p: &str| found.iter().any(|f| f == p);
    let mut out: Vec<Agent> = custom.iter().filter(|(_, c)| has(program(c))).map(|(n, c)| Agent { name: n.clone(), command: c.clone() }).collect();
    for (name, prog, command) in KNOWN {
        let taken = out.iter().any(|a| a.name.eq_ignore_ascii_case(name) || a.name.eq_ignore_ascii_case(prog));
        if has(prog) && !taken {
            out.push(Agent { name: name.to_string(), command: command.to_string() });
        }
    }
    out
}

/// `assistant = "…"` in the settings skips the menu. It may name an agent (by
/// its name or its program, any case) or simply be the command to run.
pub fn named(choice: &str, agents: &[Agent]) -> Agent {
    let wanted = choice.trim();
    let by_name = agents.iter().find(|a| a.name.eq_ignore_ascii_case(wanted) || program(&a.command).eq_ignore_ascii_case(wanted));
    let known = KNOWN.iter().find(|(n, p, _)| n.eq_ignore_ascii_case(wanted) || p.eq_ignore_ascii_case(wanted));
    match (by_name, known) {
        (Some(agent), _) => agent.clone(),
        (None, Some((name, _, command))) => Agent { name: name.to_string(), command: command.to_string() },
        (None, None) => Agent { name: program(wanted).rsplit('/').next().unwrap_or("assistant").to_string(), command: wanted.to_string() },
    }
}

/// The menu: which agent?
pub struct Chooser {
    pub agents: Vec<Agent>,
    pub selected: usize,
}

impl Chooser {
    /// Starts on the agent used last time, if it is still there.
    pub fn new(agents: Vec<Agent>, last: Option<&str>) -> Self {
        let selected = last.and_then(|l| agents.iter().position(|a| a.name == l)).unwrap_or(0);
        Chooser { agents, selected }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.agents.len().max(1) as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn chosen(&self) -> Option<&Agent> {
        self.agents.get(self.selected)
    }
}

fn memory(home: &Path) -> std::path::PathBuf {
    home.join("last-agent")
}

pub fn last_used(home: &Path) -> Option<String> {
    std::fs::read_to_string(memory(home)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn remember(home: &Path, name: &str) {
    let _ = std::fs::create_dir_all(home).and_then(|_| std::fs::write(memory(home), name));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn offers_what_is_installed_yours_first() {
        let custom = vec![("Work bot".to_string(), "workbot --chat {context}".to_string()), ("Ghost".to_string(), "not-installed-thing".to_string())];
        let agents = pick(&custom, &found(&["codex", "workbot", "claude"]));
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Work bot", "Claude Code", "Codex"], "yours, then the known ones in a fixed order; missing programs are left out");
        assert_eq!(agents[1].command, "claude --append-system-prompt {context}");
        assert!(pick(&[], &found(&[])).is_empty());
    }

    #[test]
    fn your_definition_replaces_a_known_one() {
        let custom = vec![("claude".to_string(), "claude --model opus --append-system-prompt {context}".to_string())];
        let agents = pick(&custom, &found(&["claude", "gemini"]));
        assert_eq!(agents.len(), 2);
        assert!(agents[0].command.contains("--model opus"));
        assert_eq!(agents[1].name, "Gemini");
    }

    #[test]
    fn a_fixed_assistant_can_be_a_name_or_a_command() {
        let agents = pick(&[], &found(&["claude", "codex"]));
        assert_eq!(named("codex", &agents).command, "codex {context}");
        assert_eq!(named("Claude Code", &agents).name, "Claude Code");
        assert_eq!(named("GEMINI", &agents).command, "gemini -i {context}", "a known agent, even if detection missed it");
        let custom = named("/opt/bin/mybot --talk {context}", &agents);
        assert_eq!((custom.name.as_str(), custom.command.as_str()), ("mybot", "/opt/bin/mybot --talk {context}"));
    }

    #[test]
    fn the_menu_remembers_and_wraps() {
        let agents = pick(&[], &found(&["claude", "codex", "gemini"]));
        let mut c = Chooser::new(agents.clone(), Some("Codex"));
        assert_eq!(c.chosen().unwrap().name, "Codex");
        c.step(2);
        assert_eq!(c.chosen().unwrap().name, "Claude Code");
        assert_eq!(Chooser::new(agents, Some("Gone")).selected, 0);

        let home = std::env::temp_dir().join(format!("omanote-agents-{}", std::process::id()));
        assert_eq!(last_used(&home), None);
        remember(&home, "Gemini");
        assert_eq!(last_used(&home).as_deref(), Some("Gemini"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn asks_the_login_shell_what_exists() {
        let got = installed(&["sh", "definitely-not-a-real-program-xyz"]);
        assert_eq!(got, ["sh"]);
    }
}
