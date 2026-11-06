//! How long each turn took, kept across restarts for "Worked for". ACP
//! replays a session's history without times, so rho-ui records each turn it
//! runs under its prompt's message ID, which rho gives the prompt again when
//! the session replays.
//!
//! The times live in one small text file, appended a line per turn:
//!
//! ```text
//! <prompt message id> <milliseconds>
//! ```

use std::{collections::HashMap, io::Write as _, path::PathBuf, time::Duration};

use gpui_kit::Global;

use crate::store;

/// Turn times by prompt message ID. A global: every session view reads and
/// records them.
#[derive(Debug, Default)]
pub(crate) struct TurnTimes {
    times: HashMap<String, Duration>,
    /// Where to save; `None` keeps times in memory only (tests).
    path: Option<PathBuf>,
}

impl Global for TurnTimes {}

impl TurnTimes {
    /// Loads `$XDG_STATE_HOME/rho-ui/turn-times`, or `~/.local/state/...`.
    /// A missing or unreadable file starts empty.
    pub(crate) fn load() -> Self {
        let path =
            store::xdg_dir("XDG_STATE_HOME", ".local/state").map(|dir| dir.join("turn-times"));
        let times = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse(&text))
            .unwrap_or_default();
        Self { times, path }
    }

    /// How long the turn that `prompt` started took, if rho-ui ran it.
    pub(crate) fn get(&self, prompt: &str) -> Option<Duration> {
        self.times.get(prompt).copied()
    }

    /// Records that the turn `prompt` started took `took`. Best effort on
    /// disk: a lost line only loses one turn's time.
    pub(crate) fn record(&mut self, prompt: &str, took: Duration) {
        self.times.insert(prompt.to_owned(), took);
        let Some(path) = &self.path else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let line = format!("{prompt} {}\n", took.as_millis());
        // One `write` per line, so appends from two windows don't interleave.
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
    }
}

/// Reads `<id> <ms>` lines; a later line for an ID wins. Bad lines are
/// skipped.
fn parse(text: &str) -> HashMap<String, Duration> {
    text.lines()
        .filter_map(|line| {
            let (id, ms) = line.rsplit_once(' ')?;
            Some((id.to_owned(), Duration::from_millis(ms.parse().ok()?)))
        })
        .filter(|(id, _)| !id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_time_survives_a_reload() {
        let dir = std::env::temp_dir().join(format!("rho-ui-turn-times-{}", std::process::id()));
        let path = dir.join("turn-times");
        let _ = std::fs::remove_file(&path);
        let mut times = TurnTimes {
            times: HashMap::new(),
            path: Some(path.clone()),
        };
        times.record("u1", Duration::from_millis(9_400));
        times.record("u2", Duration::from_secs(337));
        times.record("u1", Duration::from_millis(9_500));

        let text = std::fs::read_to_string(&path).expect("the times were saved");
        let reloaded = parse(&text);
        assert_eq!(reloaded.get("u1"), Some(&Duration::from_millis(9_500)));
        assert_eq!(reloaded.get("u2"), Some(&Duration::from_secs(337)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parsing_skips_bad_lines() {
        let times = parse("ok 5\nnonsense\n 7\nneg -1\nok two\n");
        assert_eq!(times.len(), 1);
        assert_eq!(times.get("ok"), Some(&Duration::from_millis(5)));
    }
}
