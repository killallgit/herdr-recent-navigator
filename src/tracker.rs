use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Context;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// The entity kind that was focused. Used to tag MRU entries so we can
/// apply pane-level, tab-level, and workspace-level timestamps independently.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum MruKind {
    Pane,
    Tab,
    Workspace,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MruEntry {
    pub kind: MruKind,
    /// The entity ID: pane_id, tab_id, or workspace_id depending on `kind`.
    pub id: String,
    /// The workspace that the focused entity belongs to.
    pub workspace_id: String,
    pub focused_at: u64,
    /// Human-readable name for the entity (tab label, pane label, workspace label).
    /// Used for debugging; populated when the caller has the data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable workspace label for context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
}

/// Return the path to the state directory, or a default fallback.
pub fn state_dir_or_default() -> PathBuf {
    std::env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/herdr-recent-navigator"))
}

/// Path to the MRU data file.
fn mru_path() -> PathBuf {
    state_dir_or_default().join("mru.json")
}

/// Path to the cross-process lock file.
fn lock_path() -> PathBuf {
    state_dir_or_default().join("mru.lock")
}

/// Acquire an exclusive lock on the lock file.
/// Blocks until the lock is acquired. Returns the locked file handle.
/// The lock is automatically released when the handle is dropped.
fn acquire_lock() -> anyhow::Result<File> {
    let path = lock_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Failed to open lock file: {}", path.display()))?;
    file.lock_exclusive()?;
    Ok(file)
}

/// A focus change as seen by a `track` process at the moment it looked.
/// `observed_at` is captured before any slow lookups so that persistence
/// order cannot rewrite focus chronology.
pub struct FocusObservation {
    kind: MruKind,
    id: String,
    workspace_id: String,
    observed_at: u64,
    name: Option<String>,
    workspace_name: Option<String>,
}

impl FocusObservation {
    pub fn new(kind: MruKind, id: &str, workspace_id: &str, observed_at: u64) -> Self {
        Self {
            kind,
            id: id.to_string(),
            workspace_id: workspace_id.to_string(),
            observed_at,
            name: None,
            workspace_name: None,
        }
    }

    pub fn named(mut self, name: Option<String>, workspace_name: Option<String>) -> Self {
        self.name = name;
        self.workspace_name = workspace_name;
        self
    }
}

pub fn now_millis() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// An observation further behind the newest entry than any hook can plausibly
/// lag is a wall-clock step backwards, not a delayed hook.
const MAX_HOOK_LAG_MILLIS: u64 = 60_000;

/// Re-time panes during an alt-tab cycle: panes in `originals` get their
/// pre-cycle `focused_at` back and `promoted` is stamped `promoted_at`, so the
/// history reads as if only the promoted pane had been focused.
pub fn restore_and_promote_pane(
    originals: &[(String, u64)],
    promoted: &str,
    promoted_at: u64,
) -> anyhow::Result<()> {
    let path = mru_path();
    let _lock = acquire_lock()?;
    let mut entries = read_entries_newest_first(&path);
    for entry in entries.iter_mut().filter(|e| e.kind == MruKind::Pane) {
        if let Some((_, focused_at)) = originals.iter().find(|(id, _)| *id == entry.id) {
            entry.focused_at = *focused_at;
        }
        if entry.id == promoted {
            entry.focused_at = promoted_at;
        }
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.focused_at));
    write_entries(&path, &entries)
}

/// Persist an observation to the MRU file.
///
/// Uses a cross-process lock file (`mru.lock`) to serialize concurrent writes
/// from multiple `track` subcommand processes that Herdr fires simultaneously.
pub fn record_observation(observation: FocusObservation) -> anyhow::Result<()> {
    let path = mru_path();
    let _lock = acquire_lock()?;

    let mut entries = read_entries_newest_first(&path);

    let FocusObservation {
        kind,
        id,
        workspace_id,
        observed_at,
        name,
        workspace_name,
    } = observation;

    let newest = entries.first().map_or(0, |e| e.focused_at);
    let observed_at = if observed_at.saturating_add(MAX_HOOK_LAG_MILLIS) < newest {
        newest
    } else {
        observed_at
    };

    let old = entries
        .iter()
        .find(|e| e.kind == kind && e.id == id)
        .cloned();
    if old.as_ref().is_some_and(|o| o.focused_at > observed_at) {
        return Ok(());
    }
    entries.retain(|e| !(e.kind == kind && e.id == id));

    let position = entries
        .iter()
        .position(|e| e.focused_at <= observed_at)
        .unwrap_or(entries.len());
    entries.insert(
        position,
        MruEntry {
            kind,
            id,
            workspace_id,
            focused_at: observed_at,
            name: name.or_else(|| old.as_ref().and_then(|o| o.name.clone())),
            workspace_name: workspace_name
                .or_else(|| old.as_ref().and_then(|o| o.workspace_name.clone())),
        },
    );

    entries.truncate(300);

    write_entries(&path, &entries)
    // Lock released when `_lock` is dropped
}

fn read_entries_newest_first(path: &Path) -> Vec<MruEntry> {
    let mut entries: Vec<MruEntry> = fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default();
    entries.sort_by_key(|e| std::cmp::Reverse(e.focused_at));
    entries
}

fn write_entries(path: &Path, entries: &[MruEntry]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write to temp file then rename for atomicity.
    // If the process crashes mid-write, the original file is untouched.
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(entries)?)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load all MRU entries from the persistent state file.
pub fn load_mru() -> Vec<MruEntry> {
    let state_path = mru_path();
    let content = match std::fs::read_to_string(&state_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    match serde_json::from_str(&content) {
        Ok(entries) => entries,
        Err(e) => {
            // Corrupted file: back up and return empty vec to avoid silent data loss
            log::error!("mru.json corrupted ({}), backing up to mru.json.bak", e);
            let bak_path = state_path.with_extension("json.bak");
            let _ = std::fs::rename(&state_path, &bak_path);
            vec![]
        }
    }
}

/// Build three separate timestamp maps (pane/tab/workspace) from MRU entries.
/// Each map returns the **most recent** focus timestamp for that entity.
///
/// These maps are passed to builders directly so each category tab
/// sorts using ONLY its own level's timestamps — no level mixing.
pub fn build_timestamp_maps(
    entries: &[MruEntry],
) -> (
    HashMap<String, u64>,
    HashMap<String, u64>,
    HashMap<String, u64>,
) {
    let mut pane_ts: HashMap<String, u64> = HashMap::new();
    let mut tab_ts: HashMap<String, u64> = HashMap::new();
    let mut ws_ts: HashMap<String, u64> = HashMap::new();

    for entry in entries {
        let ts = entry.focused_at;
        match entry.kind {
            MruKind::Pane => {
                pane_ts.entry(entry.id.clone()).or_insert(ts);
            }
            MruKind::Tab => {
                tab_ts.entry(entry.id.clone()).or_insert(ts);
            }
            MruKind::Workspace => {
                ws_ts.entry(entry.id.clone()).or_insert(ts);
            }
        }
    }

    (pane_ts, tab_ts, ws_ts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::with_temp_dir;

    fn record_now(kind: MruKind, id: &str, name: Option<String>) -> anyhow::Result<()> {
        record_observation(FocusObservation::new(kind, id, "ws-1", now_millis()).named(name, None))
    }

    // ── record_event tests ──

    #[test]
    fn test_record_event_creates_file() {
        with_temp_dir(|dir| {
            let path = dir.join("mru.json");
            assert!(!path.exists(), "File should not exist before recording");
            record_now(MruKind::Pane, "pane-1", None).unwrap();
            assert!(path.exists(), "File should exist after recording");
        });
    }

    #[test]
    fn test_record_event_updates_timestamp() {
        with_temp_dir(|_dir| {
            record_now(MruKind::Pane, "pane-1", None).unwrap();
            let entries = load_mru();
            assert_eq!(entries.len(), 1);
            let t1 = entries[0].focused_at;

            std::thread::sleep(std::time::Duration::from_millis(1));
            record_now(MruKind::Pane, "pane-1", None).unwrap();
            let entries = load_mru();
            assert_eq!(entries.len(), 1, "Duplicate should not add new entry");
            assert!(entries[0].focused_at > t1, "Timestamp should be updated");
        });
    }

    #[test]
    fn test_record_event_truncates_at_300() {
        with_temp_dir(|_dir| {
            for i in 0..301 {
                record_now(MruKind::Pane, &format!("pane-{}", i), None).unwrap();
            }
            let entries = load_mru();
            assert_eq!(entries.len(), 300, "Should truncate to 300 entries");
            assert_eq!(entries[0].id, "pane-300", "Most recent should be first");
        });
    }

    #[test]
    fn test_record_event_preserves_name_on_update() {
        with_temp_dir(|_dir| {
            record_now(MruKind::Pane, "pane-1", Some("MyPane".into())).unwrap();

            record_now(MruKind::Pane, "pane-1", None).unwrap();

            let entries = load_mru();
            assert_eq!(
                entries[0].name.as_deref(),
                Some("MyPane"),
                "Name should be preserved from previous entry"
            );
        });
    }

    // ── observation ordering tests ──

    fn observation(kind: MruKind, id: &str, observed_at: u64) -> FocusObservation {
        FocusObservation::new(kind, id, "ws-1", observed_at)
    }

    #[test]
    fn test_delayed_older_observation_stays_behind_newer_entries() {
        with_temp_dir(|_dir| {
            record_observation(observation(MruKind::Workspace, "A", 1)).unwrap();
            record_observation(observation(MruKind::Workspace, "C", 3)).unwrap();
            record_observation(observation(MruKind::Workspace, "B", 2)).unwrap();

            let ids: Vec<_> = load_mru().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, ["C", "B", "A"]);
        });
    }

    #[test]
    fn test_delayed_older_observation_does_not_demote_same_entry() {
        with_temp_dir(|_dir| {
            record_observation(
                observation(MruKind::Workspace, "A", 5).named(Some("Alpha".into()), None),
            )
            .unwrap();
            record_observation(observation(MruKind::Workspace, "A", 2)).unwrap();

            let entries = load_mru();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].focused_at, 5);
            assert_eq!(entries[0].name.as_deref(), Some("Alpha"));
        });
    }

    #[test]
    fn test_concurrent_out_of_order_observations_persist_in_observation_order() {
        with_temp_dir(|_dir| {
            let handles: Vec<_> = [3u64, 7, 1, 5, 2, 8, 4, 6]
                .into_iter()
                .map(|observed_at| {
                    std::thread::spawn(move || {
                        let id = format!("ws-{observed_at}");
                        record_observation(observation(MruKind::Workspace, &id, observed_at))
                            .unwrap();
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }

            let timestamps: Vec<_> = load_mru().into_iter().map(|e| e.focused_at).collect();
            assert_eq!(timestamps, [8, 7, 6, 5, 4, 3, 2, 1]);
        });
    }

    #[test]
    fn test_delayed_older_observation_does_not_overwrite_newer_fields() {
        with_temp_dir(|_dir| {
            record_observation(
                FocusObservation::new(MruKind::Tab, "t1", "ws-new", 5)
                    .named(Some("Newer".into()), Some("NewWs".into())),
            )
            .unwrap();
            record_observation(
                FocusObservation::new(MruKind::Tab, "t1", "ws-old", 2)
                    .named(Some("Older".into()), Some("OldWs".into())),
            )
            .unwrap();

            let entry = load_mru().remove(0);
            assert_eq!(entry.workspace_id, "ws-new");
            assert_eq!(entry.name.as_deref(), Some("Newer"));
            assert_eq!(entry.workspace_name.as_deref(), Some("NewWs"));
        });
    }

    #[test]
    fn test_observation_into_unsorted_file_lands_in_chronological_place() {
        with_temp_dir(|dir| {
            let unsorted = r#"[
                {"kind":"Workspace","id":"B","workspace_id":"B","focused_at":1},
                {"kind":"Workspace","id":"C","workspace_id":"C","focused_at":3},
                {"kind":"Workspace","id":"A","workspace_id":"A","focused_at":0}
            ]"#;
            std::fs::write(dir.join("mru.json"), unsorted).unwrap();

            record_observation(observation(MruKind::Workspace, "D", 2)).unwrap();

            let ids: Vec<_> = load_mru().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, ["C", "D", "B", "A"]);
        });
    }

    #[test]
    fn test_observation_far_behind_newest_is_treated_as_clock_regression() {
        with_temp_dir(|_dir| {
            let now = 10_000_000;
            record_observation(observation(MruKind::Workspace, "A", now - 10)).unwrap();
            record_observation(observation(MruKind::Workspace, "B", now)).unwrap();
            record_observation(observation(MruKind::Workspace, "A", now - 1_000_000)).unwrap();

            let ids: Vec<_> = load_mru().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, ["A", "B"]);
            assert_eq!(load_mru()[0].focused_at, now);
        });
    }

    #[test]
    fn promoting_a_pane_keeps_skipped_stops_in_their_original_order() {
        with_temp_dir(|_dir| {
            for (id, at) in [("A", 10), ("B", 20), ("C", 30)] {
                record_observation(observation(MruKind::Pane, id, at)).unwrap();
            }
            let originals: Vec<_> = [("C", 30), ("B", 20), ("A", 10)]
                .map(|(id, at)| (id.to_string(), at))
                .into();

            restore_and_promote_pane(&originals, "B", 100).unwrap();
            restore_and_promote_pane(&originals, "A", 200).unwrap();

            let ids: Vec<_> = load_mru().into_iter().map(|e| e.id).collect();
            assert_eq!(ids, ["A", "C", "B"]);
        });
    }

    // ── load_mru tests ──

    #[test]
    fn test_load_mru_returns_empty_on_missing_file() {
        with_temp_dir(|_dir| {
            let entries = load_mru();
            assert!(entries.is_empty(), "No file should return empty vec");
        });
    }

    #[test]
    fn test_load_mru_returns_empty_on_corrupt_file() {
        with_temp_dir(|dir| {
            let path = dir.join("mru.json");
            std::fs::write(&path, "{{{corrupted data}}}").unwrap();
            let entries = load_mru();
            assert!(entries.is_empty(), "Corrupt file should return empty vec");
            assert!(
                path.with_extension("json.bak").exists(),
                "Backup should be created for corrupt file"
            );
        });
    }

    #[test]
    fn test_load_mru_reads_valid_entries() {
        with_temp_dir(|dir| {
            let path = dir.join("mru.json");
            let data = r#"[
                {"kind":"Pane","id":"pane-1","workspace_id":"ws-1","focused_at":1000}
            ]"#;
            std::fs::write(&path, data).unwrap();
            let entries = load_mru();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, "pane-1");
        });
    }

    // ── build_timestamp_maps tests ──

    #[test]
    fn test_build_timestamp_maps_separates_kinds() {
        let entries = vec![
            MruEntry {
                kind: MruKind::Pane,
                id: "same-id".into(),
                workspace_id: "ws".into(),
                focused_at: 100,
                name: None,
                workspace_name: None,
            },
            MruEntry {
                kind: MruKind::Tab,
                id: "same-id".into(),
                workspace_id: "ws".into(),
                focused_at: 200,
                name: None,
                workspace_name: None,
            },
            MruEntry {
                kind: MruKind::Workspace,
                id: "same-id".into(),
                workspace_id: "ws".into(),
                focused_at: 300,
                name: None,
                workspace_name: None,
            },
        ];
        let (pane, tab, ws) = build_timestamp_maps(&entries);
        assert_eq!(pane.get("same-id"), Some(&100));
        assert_eq!(tab.get("same-id"), Some(&200));
        assert_eq!(ws.get("same-id"), Some(&300));
    }

    #[test]
    fn test_build_timestamp_maps_keeps_most_recent() {
        // MRU order: most recent first (999 before 100)
        let entries = vec![
            MruEntry {
                kind: MruKind::Pane,
                id: "pane-1".into(),
                workspace_id: "ws".into(),
                focused_at: 999,
                name: None,
                workspace_name: None,
            },
            MruEntry {
                kind: MruKind::Pane,
                id: "pane-1".into(),
                workspace_id: "ws".into(),
                focused_at: 100,
                name: None,
                workspace_name: None,
            },
        ];
        let (pane, _, _) = build_timestamp_maps(&entries);
        assert_eq!(
            pane.get("pane-1"),
            Some(&999),
            "Should keep most recent (first seen in MRU order)"
        );
    }

    // ── atomic write test ──

    #[test]
    fn test_record_event_atomic_write_survives_crash() {
        with_temp_dir(|dir| {
            record_now(MruKind::Pane, "pane-1", None).unwrap();
            let path = dir.join("mru.json");
            let content_before = std::fs::read_to_string(&path).unwrap();

            // Simulate partial write: tmp file exists but rename never happened
            let tmp_path = path.with_extension("json.tmp");
            std::fs::write(&tmp_path, "{{{partial}}}").unwrap();
            // Don't rename — simulates crash

            let content_after = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                content_before, content_after,
                "Crash during write should not corrupt original file"
            );
        });
    }
}
