//! Toggle state for the navigator popup, kept separately for each Herdr connection.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;

pub struct OpenedPopup {
    pub pane_id: Option<String>,
    pub exit_code: i32,
}

pub trait HerdrPopups {
    fn close(&self, pane_id: &str) -> bool;
    fn open(&self) -> anyhow::Result<OpenedPopup>;
}

pub enum Toggled {
    Closed,
    Opened(OpenedPopup),
}

pub struct PopupState {
    owner_path: PathBuf,
}

impl PopupState {
    pub fn for_connection(state_dir: &Path, socket_path: Option<&str>) -> Self {
        Self {
            owner_path: crate::connection::state_file(state_dir, socket_path, "pane", "lock"),
        }
    }

    /// Held for a whole toggle or release so concurrent callers in one
    /// connection take turns; released when the returned handle drops.
    fn serialize(&self) -> anyhow::Result<File> {
        if let Some(parent) = self.owner_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let guard = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.owner_path.with_extension("guard"))?;
        guard.lock_exclusive()?;
        Ok(guard)
    }

    pub fn toggle(&self, herdr: &dyn HerdrPopups) -> anyhow::Result<Toggled> {
        let _turn = self.serialize()?;
        if let Ok(owner) = std::fs::read_to_string(&self.owner_path) {
            let closed = herdr.close(owner.trim());
            self.forget_owner()?;
            if closed {
                return Ok(Toggled::Closed);
            }
        }
        let opened = herdr.open()?;
        if let Some(pane_id) = &opened.pane_id
            && let Err(e) = std::fs::write(&self.owner_path, pane_id)
        {
            log::error!("Failed to record popup owner {pane_id}: {e}");
        }
        Ok(Toggled::Opened(opened))
    }

    /// Clears this connection's state when `pane_id` still owns it, or
    /// unconditionally when the popup does not know its own pane id.
    pub fn release(&self, pane_id: Option<&str>) -> anyhow::Result<()> {
        let _turn = self.serialize()?;
        let owned_by_caller = pane_id.is_none_or(|pane_id| {
            std::fs::read_to_string(&self.owner_path).is_ok_and(|owner| owner.trim() == pane_id)
        });
        if owned_by_caller {
            self.forget_owner()?;
        }
        Ok(())
    }

    fn forget_owner(&self) -> anyhow::Result<()> {
        match std::fs::remove_file(&self.owner_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::TempDir;

    struct FakeHerdr {
        next_id: &'static str,
        closable: RefCell<Vec<String>>,
        closed: RefCell<Vec<String>>,
    }

    impl FakeHerdr {
        fn opening(next_id: &'static str) -> Self {
            Self {
                next_id,
                closable: RefCell::new(Vec::new()),
                closed: RefCell::new(Vec::new()),
            }
        }
    }

    impl HerdrPopups for FakeHerdr {
        fn close(&self, pane_id: &str) -> bool {
            self.closed.borrow_mut().push(pane_id.to_string());
            let mut closable = self.closable.borrow_mut();
            let known = closable.iter().any(|id| id == pane_id);
            closable.retain(|id| id != pane_id);
            known
        }

        fn open(&self) -> anyhow::Result<OpenedPopup> {
            self.closable.borrow_mut().push(self.next_id.to_string());
            Ok(OpenedPopup {
                pane_id: Some(self.next_id.to_string()),
                exit_code: 0,
            })
        }
    }

    fn owner_of(state: &PopupState) -> Option<String> {
        std::fs::read_to_string(&state.owner_path).ok()
    }

    #[test]
    fn first_toggle_opens_a_popup_and_records_its_owner() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        let herdr = FakeHerdr::opening("popup-1");

        let toggled = state.toggle(&herdr).unwrap();

        assert!(matches!(toggled, Toggled::Opened(_)));
        assert_eq!(owner_of(&state).as_deref(), Some("popup-1"));
    }

    #[test]
    fn second_toggle_closes_the_recorded_popup_and_clears_the_owner() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        let herdr = FakeHerdr::opening("popup-1");
        state.toggle(&herdr).unwrap();

        let toggled = state.toggle(&herdr).unwrap();

        assert!(matches!(toggled, Toggled::Closed));
        assert_eq!(*herdr.closed.borrow(), ["popup-1"]);
        assert_eq!(owner_of(&state), None);
    }

    #[test]
    fn stale_owner_that_cannot_be_closed_is_replaced_by_a_fresh_popup() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        std::fs::write(&state.owner_path, "ghost").unwrap();
        let herdr = FakeHerdr::opening("popup-2");

        let toggled = state.toggle(&herdr).unwrap();

        assert!(matches!(toggled, Toggled::Opened(_)));
        assert_eq!(owner_of(&state).as_deref(), Some("popup-2"));
    }

    #[test]
    fn toggling_in_another_connection_leaves_the_first_popup_alone() {
        let dir = TempDir::new().unwrap();
        let session_a = PopupState::for_connection(dir.path(), Some("/run/herdr-a.sock"));
        let session_b = PopupState::for_connection(dir.path(), Some("/run/herdr-b.sock"));
        let herdr_a = FakeHerdr::opening("session-a:popup");
        let herdr_b = FakeHerdr::opening("session-b:popup");
        session_a.toggle(&herdr_a).unwrap();

        let toggled = session_b.toggle(&herdr_b).unwrap();

        assert!(matches!(toggled, Toggled::Opened(_)));
        assert!(herdr_b.closed.borrow().is_empty());
        assert_eq!(owner_of(&session_a).as_deref(), Some("session-a:popup"));
        assert_eq!(owner_of(&session_b).as_deref(), Some("session-b:popup"));
    }

    #[test]
    fn release_only_clears_state_owned_by_the_releasing_popup() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        state.toggle(&FakeHerdr::opening("popup-1")).unwrap();

        state.release(Some("someone-else")).unwrap();
        assert_eq!(owner_of(&state).as_deref(), Some("popup-1"));

        state.release(Some("popup-1")).unwrap();
        assert_eq!(owner_of(&state), None);
    }

    struct SharedHerdr {
        open_popups: std::sync::Mutex<Vec<String>>,
        most_open_at_once: std::sync::atomic::AtomicUsize,
        opened: std::sync::atomic::AtomicUsize,
    }

    impl HerdrPopups for SharedHerdr {
        fn close(&self, pane_id: &str) -> bool {
            let mut open_popups = self.open_popups.lock().unwrap();
            let known = open_popups.iter().any(|id| id == pane_id);
            open_popups.retain(|id| id != pane_id);
            known
        }

        fn open(&self) -> anyhow::Result<OpenedPopup> {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let n = self
                .opened
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pane_id = format!("popup-{n}");
            let mut open_popups = self.open_popups.lock().unwrap();
            open_popups.push(pane_id.clone());
            self.most_open_at_once
                .fetch_max(open_popups.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(OpenedPopup {
                pane_id: Some(pane_id),
                exit_code: 0,
            })
        }
    }

    #[test]
    fn concurrent_toggles_never_leave_duplicate_popups() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        let herdr = SharedHerdr {
            open_popups: std::sync::Mutex::new(Vec::new()),
            most_open_at_once: std::sync::atomic::AtomicUsize::new(0),
            opened: std::sync::atomic::AtomicUsize::new(0),
        };

        std::thread::scope(|scope| {
            for _ in 0..7 {
                scope.spawn(|| state.toggle(&herdr).unwrap());
            }
        });

        let most_open = herdr
            .most_open_at_once
            .load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(most_open, 1);
        assert_eq!(herdr.open_popups.lock().unwrap().len(), 1);
        assert_eq!(
            owner_of(&state),
            herdr.open_popups.lock().unwrap().first().cloned()
        );
    }

    #[test]
    fn release_without_a_known_pane_clears_this_connections_state() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), Some("/run/herdr-a.sock"));
        let other = PopupState::for_connection(dir.path(), Some("/run/herdr-b.sock"));
        state.toggle(&FakeHerdr::opening("popup-1")).unwrap();
        other.toggle(&FakeHerdr::opening("popup-2")).unwrap();

        state.release(None).unwrap();

        assert_eq!(owner_of(&state), None);
        assert_eq!(owner_of(&other).as_deref(), Some("popup-2"));
    }

    #[test]
    fn failing_to_record_the_owner_still_reports_the_opened_popup() {
        let dir = TempDir::new().unwrap();
        let state = PopupState::for_connection(dir.path(), None);
        std::fs::create_dir(&state.owner_path).unwrap();

        let toggled = state.toggle(&FakeHerdr::opening("popup-1")).unwrap();

        assert!(matches!(toggled, Toggled::Opened(_)));
    }
}
