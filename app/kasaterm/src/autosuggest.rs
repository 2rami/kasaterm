//! History-backed inline autosuggestion (fish / zsh-autosuggestions
//! style "ghost text"). This module owns ONLY the data: the command
//! history (loaded from `$HISTFILE` plus the commands typed this
//! session) and the matcher that turns a typed prefix into a suggested
//! completion. The host (`main.rs`) owns the input-line tracking and
//! rendering — see `App::update_suggestion` / `draw_ghost`.
//!
//! Why a separate store: the renderer asks `suggest(prefix)` once per
//! frame, so the match has to be cheap (linear scan, most-recent-first,
//! first hit wins). Keeping history parsing here keeps that hot path
//! and the brittle zsh-history format out of the 5k-line main module.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// In-memory command history with two tiers: `session` (commands the
/// user ran in THIS window, newest first — instant feedback) and
/// `file` (parsed from `$HISTFILE`, refreshed lazily on mtime change).
/// Both are deduped and stored newest-first so `suggest` can return the
/// first match it walks into.
pub struct History {
    path: Option<PathBuf>,
    /// Parsed from the history file, newest-first, deduped.
    file: Vec<String>,
    /// Commands run this session, newest-first, deduped. Searched before
    /// `file` so a command you JUST ran suggests immediately, before the
    /// shell has even flushed it to disk.
    session: Vec<String>,
    last_mtime: Option<std::time::SystemTime>,
    last_check: Instant,
    enabled: bool,
}

impl History {
    pub fn new() -> Self {
        let enabled = std::env::var_os("KASATERM_NO_AUTOSUGGEST").is_none();
        let mut s = Self {
            path: Self::histfile(),
            file: Vec::new(),
            session: Vec::new(),
            last_mtime: None,
            // Backdated so the first maybe_refresh() actually loads.
            last_check: Instant::now() - Duration::from_secs(60),
            enabled,
        };
        if enabled {
            s.maybe_refresh();
        }
        s
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Resolve the shell history file. Honours `$HISTFILE`, else falls
    /// back to `~/.zsh_history`, else `~/.bash_history` if that's the
    /// only one present.
    fn histfile() -> Option<PathBuf> {
        if let Some(h) = std::env::var_os("HISTFILE") {
            let p = PathBuf::from(h);
            if !p.as_os_str().is_empty() {
                return Some(p);
            }
        }
        let home = std::env::var_os("HOME")?;
        let zsh = PathBuf::from(&home).join(".zsh_history");
        if zsh.exists() {
            return Some(zsh);
        }
        let bash = PathBuf::from(&home).join(".bash_history");
        if bash.exists() {
            return Some(bash);
        }
        // Default to the zsh path even if absent — it may appear later
        // and maybe_refresh() will pick it up.
        Some(zsh)
    }

    /// Re-read the history file if its mtime changed. Throttled to ~2s
    /// so a 60Hz render loop doesn't stat the file every frame.
    pub fn maybe_refresh(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_check) < Duration::from_secs(2) {
            return;
        }
        self.last_check = now;
        let Some(path) = self.path.clone() else { return };
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let Ok(mtime) = meta.modified() else { return };
        if Some(mtime) == self.last_mtime {
            return;
        }
        self.last_mtime = Some(mtime);
        self.file = Self::load_file(&path);
    }

    fn load_file(path: &PathBuf) -> Vec<String> {
        let Ok(bytes) = std::fs::read(path) else {
            return Vec::new();
        };
        let content = String::from_utf8_lossy(&bytes);
        Self::parse(&content)
    }

    /// Parse a zsh/bash history file into commands, newest-first, deduped.
    ///
    /// zsh extended-history lines look like `: <ts>:<elapsed>;<command>`;
    /// plain bash lines are just the command. A trailing backslash means
    /// the command continued onto the next physical line.
    fn parse(content: &str) -> Vec<String> {
        let mut cmds: Vec<String> = Vec::new();
        let mut cont: Option<String> = None;
        for raw in content.split('\n') {
            if let Some(prev) = cont.take() {
                let joined = format!("{}\n{}", prev.trim_end_matches('\\'), raw);
                if raw.ends_with('\\') {
                    cont = Some(joined);
                } else {
                    cmds.push(joined);
                }
                continue;
            }
            if raw.is_empty() {
                continue;
            }
            // Strip the ": ts:elapsed;" metadata prefix if present.
            let cmd = if raw.starts_with(": ") {
                raw.splitn(2, ';').nth(1).unwrap_or(raw)
            } else {
                raw
            };
            if cmd.ends_with('\\') {
                cont = Some(cmd.to_string());
            } else {
                cmds.push(cmd.to_string());
            }
        }
        // File is oldest-first; walk back to front so the first time we
        // see a command (= its most recent run) is the one we keep.
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out = Vec::with_capacity(cmds.len());
        for cmd in cmds.iter().rev() {
            let c = cmd.trim();
            if c.is_empty() {
                continue;
            }
            if seen.insert(c) {
                out.push(c.to_string());
            }
        }
        out
    }

    /// Remember a command the user just submitted so it suggests
    /// instantly. Multiline commands are skipped — ghost text is a
    /// single-row overlay.
    pub fn record(&mut self, cmd: &str) {
        if !self.enabled {
            return;
        }
        let c = cmd.trim();
        if c.is_empty() || c.contains('\n') {
            return;
        }
        self.session.retain(|x| x != c);
        self.session.insert(0, c.to_string());
        if self.session.len() > 300 {
            self.session.truncate(300);
        }
    }

    /// Given the typed prefix, return the suggested *remainder* (the part
    /// after the prefix), or None if nothing in history extends it.
    /// Session history wins over the file; within each, newest wins.
    /// Multiline entries are ignored so the ghost stays one row.
    pub fn suggest(&self, input: &str) -> Option<String> {
        if !self.enabled || input.is_empty() {
            return None;
        }
        self.session
            .iter()
            .chain(self.file.iter())
            .find(|cmd| {
                cmd.len() > input.len() && !cmd.contains('\n') && cmd.starts_with(input)
            })
            .map(|cmd| cmd[input.len()..].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(file: &[&str], session: &[&str]) -> History {
        History {
            path: None,
            file: file.iter().map(|s| s.to_string()).collect(),
            session: session.iter().map(|s| s.to_string()).collect(),
            last_mtime: None,
            last_check: Instant::now(),
            enabled: true,
        }
    }

    #[test]
    fn parse_strips_extended_prefix_and_dedupes_newest_first() {
        let raw = ": 1700000000:0;git status\n\
                   : 1700000001:0;cargo build\n\
                   : 1700000002:0;git status\n";
        let parsed = History::parse(raw);
        // Newest occurrence wins; "git status" most recent → first.
        assert_eq!(parsed, vec!["git status", "cargo build"]);
    }

    #[test]
    fn parse_handles_plain_bash_lines() {
        let raw = "ls -la\ncd /tmp\nls -la\n";
        let parsed = History::parse(raw);
        assert_eq!(parsed, vec!["ls -la", "cd /tmp"]);
    }

    #[test]
    fn parse_joins_backslash_continuations() {
        let raw = ": 1:0;echo one \\\ntwo\nls\n";
        let parsed = History::parse(raw);
        assert_eq!(parsed, vec!["ls", "echo one \ntwo"]);
    }

    #[test]
    fn suggest_returns_remainder_and_session_wins() {
        let h = store(&["git status", "git stash"], &["git switch main"]);
        // Session is searched first.
        assert_eq!(h.suggest("git s"), Some("witch main".to_string()));
        // Falls through to file when session has no match.
        assert_eq!(h.suggest("git st"), Some("atus".to_string()));
    }

    #[test]
    fn suggest_rejects_exact_and_empty() {
        let h = store(&["cargo build"], &[]);
        assert_eq!(h.suggest("cargo build"), None); // exact, no remainder
        assert_eq!(h.suggest(""), None);
        assert_eq!(h.suggest("zzz"), None); // no match
    }

    #[test]
    fn record_promotes_to_front_and_dedupes() {
        let mut h = store(&[], &["b", "a"]);
        h.record("a");
        assert_eq!(h.session, vec!["a", "b"]);
        assert_eq!(h.suggest("a"), None); // "a" is exact, no remainder
    }
}
