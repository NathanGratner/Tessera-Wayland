//! Applying `config.toml` to the running compositor (design §6).
//!
//! Everything is validated before anything is changed: a reload either takes
//! effect completely or leaves the compositor exactly as it was.

use tessera_cells::Font;
use tessera_config::{Apply, ConfigValues, config_path, split_command};
use tessera_ipc::Response;

use crate::{
    input::Bindings,
    layout::LayoutOptions,
    state::{ModKey, Tessera},
};

/// Settings read on every event rather than looked up in [`ConfigValues`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behaviour {
    /// Program and arguments for Mod+Return and the launcher's terminal entry.
    pub terminal: Vec<String>,
    /// No gaps when a workspace holds a single window.
    pub smart_gaps: bool,
    /// New windows take keyboard focus.
    pub focus_new_windows: bool,
    /// Focus the window under the pointer as it moves.
    pub focus_follows_mouse: bool,
    /// ...but not when the pointer is over the launcher.
    pub follows_mouse_skips_launcher: bool,
    /// `QT_QPA_PLATFORMTHEME` for programs Tessera starts; empty leaves it unset.
    pub qt_platform_theme: String,
    /// `GTK_THEME` for programs Tessera starts; empty leaves it unset.
    pub gtk_theme: String,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            terminal: vec!["foot".into()],
            smart_gaps: false,
            focus_new_windows: true,
            focus_follows_mouse: false,
            follows_mouse_skips_launcher: true,
            qt_platform_theme: String::new(),
            gtk_theme: String::new(),
        }
    }
}

impl Tessera {
    /// Loads the configuration file at startup. A missing file means defaults;
    /// a broken one is reported and defaults are used, so a typo never stops
    /// the session from starting.
    pub fn load_initial_config(&mut self) {
        let path = config_path();
        let values = match ConfigValues::load(&path) {
            Ok(values) => values,
            Err(err) => {
                tracing::error!(error = %err, "config.toml is unusable; starting with defaults");
                ConfigValues::default()
            }
        };
        if let Err(message) = self.apply_config(values) {
            tracing::error!(
                message,
                "config.toml could not be applied; starting with defaults"
            );
            let defaults = ConfigValues::default();
            self.apply_config(defaults)
                .expect("the default configuration always applies");
        }
        tracing::info!(path = %path.display(), "configuration loaded");
    }

    /// Makes a configuration the running one.
    ///
    /// Everything that can fail (key names, the terminal command) is checked
    /// first, so an error leaves the current settings untouched.
    pub fn apply_config(&mut self, values: ConfigValues) -> Result<(), String> {
        // Checks every key name now; the table itself is built by `rebuild_bindings`
        // below, once the scripts' keys can be merged in.
        Bindings::from_config(&values)?;

        let terminal = split_command(&values.text("general.terminal"));
        if terminal.is_empty() {
            return Err("general.terminal is empty; set it to a terminal such as foot".into());
        }

        let mod_key = match values.text("general.mod_key").as_str() {
            "alt" => ModKey::Alt,
            "super" => ModKey::Super,
            _ => ModKey::for_backend(self.nested),
        };

        let snap = if values.bool("layout.snap_to_cells") {
            cell_size(&values)
        } else {
            None
        };

        // Nothing below can fail.
        self.mod_key = mod_key;
        self.layout_opts = LayoutOptions {
            gaps: values.int("layout.gaps") as i32,
            snap,
        };
        self.behaviour = Behaviour {
            terminal,
            smart_gaps: values.bool("layout.smart_gaps"),
            focus_new_windows: values.bool("focus.new_windows"),
            focus_follows_mouse: values.bool("focus.follows_mouse"),
            follows_mouse_skips_launcher: values.visible("focus.follows_mouse_skips_launcher")
                && values.bool("focus.follows_mouse_skips_launcher"),
            qt_platform_theme: values.text("general.qt_platform_theme"),
            gtk_theme: values.text("general.gtk_theme"),
        };
        self.settings = values;
        self.rebuild_bindings();
        self.apply_display_modes();
        self.apply_layout();
        Ok(())
    }

    /// Handles `ReloadConfig`: re-reads the file and reports what took effect.
    pub fn reload_config(&mut self) -> Response {
        let values = match ConfigValues::load(&config_path()) {
            Ok(values) => values,
            Err(err) => {
                return Response::Error {
                    message: err.to_string(),
                };
            }
        };

        let changes = values.changes_from(&self.settings);
        if let Err(message) = self.apply_config(values) {
            return Response::Error { message };
        }

        let (live, needs_restart): (Vec<_>, Vec<_>) = changes
            .into_iter()
            .partition(|change| change.apply == Apply::Live);
        tracing::info!(
            live = live.len(),
            restart = needs_restart.len(),
            "configuration reloaded"
        );
        Response::ConfigApplied {
            live: live.into_iter().map(|change| change.key).collect(),
            needs_restart: needs_restart.into_iter().map(|change| change.key).collect(),
        }
    }
}

/// The launcher's cell size, which snapping rounds tiles to.
///
/// Measured with the same font code the launcher uses, so the two always agree.
fn cell_size(values: &ConfigValues) -> Option<(i32, i32)> {
    let family = values.text("launcher.font");
    let size = values.int("launcher.font_size") as f32;
    match Font::load(&family, size) {
        Ok(font) => {
            let metrics = font.metrics();
            tracing::debug!(
                family,
                size,
                width = metrics.width,
                height = metrics.height,
                "cell size"
            );
            Some((metrics.width as i32, metrics.height as i32))
        }
        Err(err) => {
            tracing::warn!(%err, "cannot measure the launcher font; not snapping to cells");
            None
        }
    }
}
