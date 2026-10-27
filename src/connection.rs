//! Per-Herdr-connection state files, so sessions sharing a state directory
//! never read or overwrite each other's state.

use std::path::{Path, PathBuf};

pub fn socket_path_from_env() -> Option<String> {
    std::env::var("HERDR_SOCKET_PATH")
        .ok()
        .filter(|s| !s.is_empty())
}

/// `<stem>-<hash of socket>.<extension>`, or `<stem>.<extension>` without a socket.
pub fn state_file(
    state_dir: &Path,
    socket_path: Option<&str>,
    stem: &str,
    extension: &str,
) -> PathBuf {
    let file_name = match socket_path {
        Some(socket) => format!("{stem}-{:016x}.{extension}", fnv1a(socket)),
        None => format!("{stem}.{extension}"),
    };
    state_dir.join(file_name)
}

/// Stable across builds, unlike `DefaultHasher`, so the process that opens a
/// popup or starts a cycle and the ones that later read it always agree.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_sockets_get_different_files_in_one_state_dir() {
        let dir = Path::new("/state");

        let a = state_file(dir, Some("/run/a.sock"), "pane", "lock");
        let b = state_file(dir, Some("/run/b.sock"), "pane", "lock");

        assert_ne!(a, b);
        assert_eq!(a.parent(), Some(dir));
    }

    #[test]
    fn the_same_socket_always_maps_to_the_same_file() {
        let dir = Path::new("/state");

        let first = state_file(dir, Some("/run/a.sock"), "cycle", "json");
        let again = state_file(dir, Some("/run/a.sock"), "cycle", "json");

        assert_eq!(first, again);
    }
}
