//! Linux startup policy. Explicit environment overrides always win.

#[cfg(any(target_os = "linux", test))]
const DEFAULTS: &[&str] = &[
    "GPUI_EARLY_PRESENT",
    "GPUI_BACKGROUND_FONTS",
    "GPUI_VULKAN_FIRST",
    "JCODE_DESKTOP_EARLY_WINDOW",
];

#[cfg(any(target_os = "linux", test))]
fn missing_defaults(mut exists: impl FnMut(&str) -> bool) -> Vec<&'static str> {
    DEFAULTS
        .iter()
        .copied()
        .filter(|key| !exists(key))
        .collect()
}

/// Must be called at main entry, before diagnostics, GPUI, or any worker starts.
pub fn configure() {
    #[cfg(target_os = "linux")]
    for key in missing_defaults(|key| std::env::var_os(key).is_some()) {
        // SAFETY: the host calls this before starting any threads or libraries
        // that read environment variables concurrently. Never call on reload.
        unsafe { std::env::set_var(key, "1") };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_launch_enables_all_startup_phases() {
        assert_eq!(missing_defaults(|_| false), DEFAULTS);
    }

    #[test]
    fn explicit_overrides_are_not_replaced() {
        assert!(missing_defaults(|_| true).is_empty());
        assert_eq!(
            missing_defaults(|key| key != "GPUI_EARLY_PRESENT"),
            ["GPUI_EARLY_PRESENT"]
        );
    }
}
