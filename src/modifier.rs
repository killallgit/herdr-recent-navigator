//! Whether the Alt/Option key is physically held right now.

#[cfg(target_os = "macos")]
mod platform {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state_id: i32) -> u64;
    }

    const COMBINED_SESSION_STATE: i32 = 0;
    const ALTERNATE_FLAG: u64 = 0x0008_0000;

    pub fn alt_held() -> Option<bool> {
        // SAFETY: reads the session's modifier flags; takes and keeps nothing.
        let flags = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) };
        Some(flags & ALTERNATE_FLAG != 0)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn alt_held() -> Option<bool> {
        None
    }
}

/// `None` where the platform cannot report modifier state.
pub use platform::alt_held;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn macos_can_report_the_modifier_state() {
        assert!(alt_held().is_some());
    }
}
