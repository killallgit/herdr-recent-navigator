//! alt-tab style cycling back through panes in recently used order.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub const IDLE_MILLIS: u64 = 1500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    pub pane_id: String,
    pub focused_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Older,
    Newer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    stops: Vec<Stop>,
    cursor: usize,
    visited: Vec<String>,
    last_press_at: u64,
}

impl Session {
    pub fn advance(
        previous: Option<Session>,
        live_mru: Vec<Stop>,
        direction: Direction,
        now: u64,
    ) -> Option<Session> {
        let session = previous.filter(|s| s.is_active(now)).unwrap_or(Session {
            stops: live_mru,
            cursor: 0,
            visited: Vec::new(),
            last_press_at: now,
        });
        let count = session.stops.len();
        if count == 0 {
            return None;
        }
        let cursor = match direction {
            Direction::Older => (session.cursor + 1) % count,
            Direction::Newer => (session.cursor + count - 1) % count,
        };
        let mut visited = session.visited;
        visited.push(session.stops[cursor].pane_id.clone());
        Some(Session {
            cursor,
            visited,
            last_press_at: now,
            ..session
        })
    }

    pub fn is_active(&self, now: u64) -> bool {
        now.saturating_sub(self.last_press_at) <= IDLE_MILLIS
    }

    /// Focus events for panes this cycle moved to are its own: the track hook
    /// must not record them, or intermediate stops would reorder history.
    pub fn owns_focus(&self, pane_id: &str, now: u64) -> bool {
        self.is_active(now) && self.visited.iter().any(|visited| visited == pane_id)
    }

    /// Every stop with the `focused_at` it had before the cycle began.
    pub fn originals(&self) -> Vec<(String, u64)> {
        self.stops
            .iter()
            .map(|stop| (stop.pane_id.clone(), stop.focused_at))
            .collect()
    }

    pub fn target(&self) -> &Stop {
        &self.stops[self.cursor]
    }
}

/// The persisted cycle session of one plugin state directory.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn in_dir(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join("cycle.json"),
        }
    }

    /// Held across a whole cycle press so rapid presses take turns.
    pub fn lock(&self) -> anyhow::Result<File> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let guard = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))?;
        guard.lock_exclusive()?;
        Ok(guard)
    }

    pub fn load(&self) -> Option<Session> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn save(&self, session: &Session) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, serde_json::to_string(session)?)?;
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mru(ids: &[&str]) -> Vec<Stop> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| Stop {
                pane_id: id.to_string(),
                focused_at: 1000 - i as u64,
            })
            .collect()
    }

    fn target_id(session: &Session) -> &str {
        &session.target().pane_id
    }

    #[test]
    fn first_older_press_targets_the_second_most_recent_pane() {
        let session =
            Session::advance(None, mru(&["A", "B", "C"]), Direction::Older, 5000).unwrap();

        assert_eq!(target_id(&session), "B");
    }

    #[test]
    fn presses_within_the_idle_window_keep_stepping_older() {
        let first = Session::advance(None, mru(&["A", "B", "C"]), Direction::Older, 5000).unwrap();

        let second =
            Session::advance(Some(first), mru(&["B", "A", "C"]), Direction::Older, 5100).unwrap();

        assert_eq!(target_id(&second), "C");
    }

    #[test]
    fn newer_press_steps_back_toward_the_origin() {
        let first = Session::advance(None, mru(&["A", "B", "C"]), Direction::Older, 5000).unwrap();
        let second = Session::advance(Some(first), mru(&[]), Direction::Older, 5100).unwrap();

        let third = Session::advance(Some(second), mru(&[]), Direction::Newer, 5200).unwrap();

        assert_eq!(target_id(&third), "B");
    }

    #[test]
    fn stepping_past_either_end_wraps_around() {
        let from_origin_newer =
            Session::advance(None, mru(&["A", "B", "C"]), Direction::Newer, 5000).unwrap();
        let wrapped_older = Session::advance(
            Some(from_origin_newer.clone()),
            mru(&[]),
            Direction::Older,
            5100,
        )
        .unwrap();

        assert_eq!(target_id(&from_origin_newer), "C");
        assert_eq!(target_id(&wrapped_older), "A");
    }

    #[test]
    fn a_single_pane_stays_put() {
        let session = Session::advance(None, mru(&["A"]), Direction::Older, 5000).unwrap();

        assert_eq!(target_id(&session), "A");
    }

    #[test]
    fn no_live_panes_means_nothing_to_cycle_to() {
        let session = Session::advance(None, mru(&[]), Direction::Older, 5000);

        assert!(session.is_none());
    }

    #[test]
    fn a_press_after_the_idle_window_starts_a_fresh_cycle() {
        let first = Session::advance(None, mru(&["A", "B", "C"]), Direction::Older, 5000).unwrap();

        let later = Session::advance(
            Some(first),
            mru(&["B", "A", "C"]),
            Direction::Older,
            5000 + IDLE_MILLIS + 1,
        )
        .unwrap();

        assert_eq!(target_id(&later), "A");
    }

    #[test]
    fn focus_on_a_visited_pane_is_ours_only_while_the_cycle_is_active() {
        let first = Session::advance(None, mru(&["A", "B", "C"]), Direction::Older, 5000).unwrap();
        let second = Session::advance(Some(first), mru(&[]), Direction::Older, 5100).unwrap();

        assert!(second.owns_focus("B", 5200));
        assert!(second.owns_focus("C", 5200));
        assert!(!second.owns_focus("D", 5200));
        assert!(!second.owns_focus("C", 5100 + IDLE_MILLIS + 1));
    }

    #[test]
    fn a_saved_session_loads_back_and_clears() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::in_dir(dir.path());
        let session = Session::advance(None, mru(&["A", "B"]), Direction::Older, 5000).unwrap();

        store.save(&session).unwrap();
        let loaded = store.load().unwrap();
        store.clear();

        assert_eq!(target_id(&loaded), "B");
        assert!(store.load().is_none());
    }
}
