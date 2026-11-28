//! Dekubi X notes: the live list of current goals, shown by `dekubi-x run`
//! (at start and exit) and by `dekubi-x notes`. Finished and idle goals are
//! hidden. The repository's `NOTES.toml` is built in; a `notes.toml` in the
//! user's data directory, when present, replaces it.
//!
//! The on-screen overlay follows once the presenter draws its own layer
//! (Phase 4, `ally-gpu` swap chain); until then the notes go to the console.

use serde::Deserialize;

/// `NOTES.toml` at the repository root, as built.
pub const BUILT_IN: &str = include_str!("../../../NOTES.toml");

/// What a goal is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Being worked on: shown.
    Active,
    /// Planned next: shown.
    Next,
    /// Waiting, nothing happening: hidden.
    Idle,
    /// Finished: hidden.
    Done,
}

/// One goal.
#[derive(Debug, Clone, Deserialize)]
pub struct Goal {
    pub title: String,
    pub state: State,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default, rename = "goal")]
    goals: Vec<Goal>,
}

/// Parses a notes file.
pub fn parse(text: &str) -> Result<Vec<Goal>, toml::de::Error> {
    Ok(toml::from_str::<File>(text)?.goals)
}

/// The notes in effect: the user's `notes.toml` in `data_dir` if it parses,
/// else the built-in list.
pub fn load(data_dir: &std::path::Path) -> Vec<Goal> {
    std::fs::read_to_string(data_dir.join("notes.toml"))
        .ok()
        .and_then(|t| parse(&t).ok())
        .unwrap_or_else(|| parse(BUILT_IN).unwrap_or_default())
}

/// The goals to show, active first, then next.
pub fn visible(goals: &[Goal]) -> Vec<&Goal> {
    let mut v: Vec<&Goal> = goals
        .iter()
        .filter(|g| matches!(g.state, State::Active | State::Next))
        .collect();
    v.sort_by_key(|g| g.state != State::Active);
    v
}

/// The notes as a boxed block of text.
pub fn render(goals: &[Goal]) -> String {
    let mut lines = vec!["Dekubi X notes".to_owned()];
    let shown = visible(goals);
    if shown.is_empty() {
        lines.push("  (no open goals)".to_owned());
    }
    for g in shown {
        let mark = if g.state == State::Active { '>' } else { '-' };
        lines.push(format!("{mark} {}", g.title));
        if !g.detail.is_empty() {
            lines.push(format!("    {}", g.detail));
        }
    }
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let bar = "-".repeat(width + 2);
    let mut out = format!("+{bar}+\n");
    for l in &lines {
        let pad = width - l.chars().count();
        out.push_str(&format!("| {l}{} |\n", " ".repeat(pad)));
    }
    out.push_str(&format!("+{bar}+"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_parses() {
        assert!(!parse(BUILT_IN).unwrap().is_empty());
    }

    #[test]
    fn done_and_idle_are_hidden_active_first() {
        let g = parse(
            "[[goal]]\ntitle = \"a\"\nstate = \"done\"\n\
             [[goal]]\ntitle = \"b\"\nstate = \"next\"\n\
             [[goal]]\ntitle = \"c\"\nstate = \"idle\"\n\
             [[goal]]\ntitle = \"d\"\nstate = \"active\"\n",
        )
        .unwrap();
        let v: Vec<&str> = visible(&g).iter().map(|g| g.title.as_str()).collect();
        assert_eq!(v, ["d", "b"]);
        assert!(render(&g).contains("> d"));
    }
}
