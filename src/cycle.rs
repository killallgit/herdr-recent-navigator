//! alt-tab style cycling back through panes in recently used order.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::tracker::EntryTime;

pub const IDLE_MILLIS: u64 = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Older,
    Newer,
}

/// What a cycle press can see of Herdr and the MRU history right now.
pub struct Sighting {
    /// Live panes, most recently used first.
    pub live_mru: Vec<String>,
    pub focused_pane: Option<String>,
    /// MRU timestamps as they stand before this press.
    pub baseline: Vec<EntryTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Panes in the order they had when the cycle began; index 0 is the origin.
    stops: Vec<String>,
    baseline: Vec<EntryTime>,
    cursor: usize,
    visited: Vec<String>,
    last_press_at: u64,
}

impl Session {
    pub fn advance(
        previous: Option<Session>,
        sighting: Sighting,
        direction: Direction,
        now: u64,
    ) -> Option<Session> {
        let session = match previous.filter(|s| s.is_active(now)) {
            Some(session) => session.without_closed_panes(&sighting.live_mru),
            None => Session::begin(sighting, now),
        };
        session.step(direction, now)
    }

    fn begin(sighting: Sighting, now: u64) -> Session {
        let mut stops = sighting.live_mru;
        if let Some(focused) = sighting.focused_pane {
            stops.retain(|pane_id| *pane_id != focused);
            stops.insert(0, focused);
        }
        Session {
            stops,
            baseline: sighting.baseline,
            cursor: 0,
            visited: Vec::new(),
            last_press_at: now,
        }
    }

    fn without_closed_panes(mut self, live: &[String]) -> Session {
        let is_live = |pane_id: &String| live.contains(pane_id);
        let current = self.stops.get(self.cursor).cloned();
        let live_before_cursor = self.stops[..self.cursor.min(self.stops.len())]
            .iter()
            .filter(|pane_id| is_live(pane_id))
            .count();
        self.stops.retain(is_live);
        self.cursor = match current {
            Some(current) if self.stops.contains(&current) => live_before_cursor,
            _ => live_before_cursor.saturating_sub(1),
        };
        self
    }

    fn step(self, direction: Direction, now: u64) -> Option<Session> {
        let count = self.stops.len();
        if count == 0 {
            return None;
        }
        let cursor = match direction {
            Direction::Older => (self.cursor + 1) % count,
            Direction::Newer => (self.cursor + count - 1) % count,
        };
        let mut visited = self.visited;
        visited.push(self.stops[cursor].clone());
        Some(Session {
            cursor,
            visited,
            last_press_at: now,
            stops: self.stops,
            baseline: self.baseline,
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

    pub fn baseline(&self) -> &[EntryTime] {
        &self.baseline
    }

    pub fn target(&self) -> &str {
        &self.stops[self.cursor]
    }
}

/// The persisted cycle session of one Herdr connection.
pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn for_connection(state_dir: &Path, socket_path: Option<&str>) -> Self {
        Self {
            path: crate::connection::state_file(state_dir, socket_path, "cycle", "json"),
        }
    }

    /// Held across a whole cycle press, and by the track hook while it decides
    /// whether a focus event is the cycle's own, so neither sees half a press.
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

    fn ids(panes: &[&str]) -> Vec<String> {
        panes.iter().map(|id| id.to_string()).collect()
    }

    /// Focused on the most recently used pane, as when the hook is up to date.
    fn see(panes: &[&str]) -> Sighting {
        Sighting {
            live_mru: ids(panes),
            focused_pane: panes.first().map(|id| id.to_string()),
            baseline: Vec::new(),
        }
    }

    fn begin(panes: &[&str], direction: Direction) -> Session {
        Session::advance(None, see(panes), direction, 5000).unwrap()
    }

    fn press(session: Session, panes: &[&str], direction: Direction, now: u64) -> Session {
        Session::advance(Some(session), see(panes), direction, now).unwrap()
    }

    #[test]
    fn first_older_press_targets_the_second_most_recent_pane() {
        let session = begin(&["A", "B", "C"], Direction::Older);

        assert_eq!(session.target(), "B");
    }

    #[test]
    fn presses_within_the_idle_window_keep_stepping_older() {
        let first = begin(&["A", "B", "C"], Direction::Older);

        let second = press(first, &["B", "A", "C"], Direction::Older, 5100);

        assert_eq!(second.target(), "C");
    }

    #[test]
    fn newer_press_steps_back_toward_the_origin() {
        let first = begin(&["A", "B", "C"], Direction::Older);
        let second = press(first, &["B", "A", "C"], Direction::Older, 5100);

        let third = press(second, &["C", "A", "B"], Direction::Newer, 5200);

        assert_eq!(third.target(), "B");
    }

    #[test]
    fn stepping_past_either_end_wraps_around() {
        let from_origin_newer = begin(&["A", "B", "C"], Direction::Newer);
        let wrapped_older = press(
            from_origin_newer.clone(),
            &["C", "A", "B"],
            Direction::Older,
            5100,
        );

        assert_eq!(from_origin_newer.target(), "C");
        assert_eq!(wrapped_older.target(), "A");
    }

    #[test]
    fn a_single_pane_stays_put() {
        let session = begin(&["A"], Direction::Older);

        assert_eq!(session.target(), "A");
    }

    #[test]
    fn no_live_panes_means_nothing_to_cycle_to() {
        let session = Session::advance(None, see(&[]), Direction::Older, 5000);

        assert!(session.is_none());
    }

    #[test]
    fn a_press_after_the_idle_window_starts_a_fresh_cycle() {
        let first = begin(&["A", "B", "C"], Direction::Older);

        let later = press(
            first,
            &["B", "A", "C"],
            Direction::Older,
            5000 + IDLE_MILLIS + 1,
        );

        assert_eq!(later.target(), "A");
    }

    #[test]
    fn the_origin_is_the_focused_pane_even_when_history_lags() {
        let lagging = Sighting {
            focused_pane: Some("B".to_string()),
            ..see(&["A", "B", "C"])
        };

        let session = Session::advance(None, lagging, Direction::Older, 5000).unwrap();

        assert_eq!(session.target(), "A");
    }

    #[test]
    fn a_focused_pane_missing_from_history_is_still_the_origin() {
        let untracked = Sighting {
            focused_pane: Some("X".to_string()),
            ..see(&["A", "B"])
        };

        let session = Session::advance(None, untracked, Direction::Older, 5000).unwrap();

        assert_eq!(session.target(), "A");
    }

    #[test]
    fn a_pane_closed_mid_cycle_is_never_targeted() {
        let first = begin(&["A", "B", "C"], Direction::Older);

        let second = press(first, &["A", "B"], Direction::Older, 5100);

        assert_eq!(second.target(), "A");
    }

    #[test]
    fn closing_the_current_stop_continues_from_its_neighbour() {
        let first = begin(&["A", "B", "C"], Direction::Older);

        let second = press(first, &["A", "C"], Direction::Older, 5100);

        assert_eq!(second.target(), "C");
    }

    #[test]
    fn focus_on_a_visited_pane_is_ours_only_while_the_cycle_is_active() {
        let first = begin(&["A", "B", "C"], Direction::Older);
        let second = press(first, &["B", "A", "C"], Direction::Older, 5100);

        assert!(second.owns_focus("B", 5200));
        assert!(second.owns_focus("C", 5200));
        assert!(!second.owns_focus("D", 5200));
        assert!(!second.owns_focus("C", 5100 + IDLE_MILLIS + 1));
    }

    #[test]
    fn a_saved_session_loads_back_and_clears() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::for_connection(dir.path(), None);

        store.save(&begin(&["A", "B"], Direction::Older)).unwrap();
        let loaded = store.load().unwrap();
        store.clear();

        assert_eq!(loaded.target(), "B");
        assert!(store.load().is_none());
    }

    #[test]
    fn connections_sharing_a_state_dir_keep_separate_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = Store::for_connection(dir.path(), Some("/run/a.sock"));
        let b = Store::for_connection(dir.path(), Some("/run/b.sock"));

        a.save(&begin(&["A", "B"], Direction::Older)).unwrap();

        assert!(b.load().is_none());
    }
}
