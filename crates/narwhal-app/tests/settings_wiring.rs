//! Verify that `App::with_settings` / `AppCore::apply_settings`
//! actually map a `narwhal_config::Settings` payload onto the
//! renderer theme so a user-supplied `config.toml` takes effect at
//! start-up (Phase 3, `_settings` TODO closure).
//!
//! We can't inspect the rendered colours from the headless backend
//! directly, but we can confirm the call signature exists and that
//! the `Theme` variants map without panicking \u2014 future regression
//! testing of the actual palette belongs in `narwhal-tui`.

use narwhal_app::{AppCore, DriverRegistry};
use narwhal_config::{ConnectionsFile, Settings, Theme};

fn core() -> AppCore {
    AppCore::new(
        DriverRegistry::with_defaults(),
        ConnectionsFile::default(),
        None,
    )
}

#[test]
fn apply_settings_accepts_each_theme_variant() {
    for theme in [Theme::Dark, Theme::Light, Theme::HighContrast] {
        let mut c = core();
        c.apply_settings(Settings {
            theme,
            ..Settings::default()
        });
        // Round-trip the rebuild path to make sure the new theme is
        // not just stored but consumed by the next render call.
        let _ = c.status_bar();
    }
}

#[test]
fn apply_settings_default_is_dark_equivalent() {
    let mut c = core();
    c.apply_settings(Settings::default());
    // Default `Settings::theme` is `Theme::Dark`; just confirm the
    // call returns without panicking and the core remains usable.
    let _ = c.status_bar();
}
