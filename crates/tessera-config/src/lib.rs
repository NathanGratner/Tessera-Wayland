//! Tessera's settings: the schema, its values, and where they live (design §6).
//!
//! The schema is data (`schema.toml`), in the spirit of the kernel's Kconfig:
//! it lists every setting with its type, default, constraints and help, and
//! the launcher's configuration menu is generated from it. Values live in
//! `config.toml` and are edited through `toml_edit`, so saving never throws
//! away a user's comments.
//!
//! ```
//! use tessera_config::{ConfigValues, Value};
//!
//! let mut values = ConfigValues::parse("[layout]\ngaps = 12\n").unwrap();
//! assert_eq!(values.int("layout.gaps"), 12);
//!
//! // Constraints come from the schema, and errors name them.
//! let err = values.set("layout.gaps", Value::Int(999)).unwrap_err();
//! assert!(err.reason.contains("0–64"));
//! ```

#![warn(missing_docs)]

pub mod binding;
pub mod expr;
pub mod schema;
pub mod scripts;
pub mod values;

use std::path::PathBuf;

pub use binding::{Binding, BindingError};
pub use expr::Expr;
pub use schema::{Apply, Entry, Kind, MenuDef, OptionDef, Pattern, Schema, Value};
pub use values::{Change, ConfigError, ConfigValues, LoadError};

/// Tessera's configuration folder: `$XDG_CONFIG_HOME/tessera`, or
/// `~/.config/tessera`. Holds `config.toml`, `services.toml` and `scripts/`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("tessera")
}

/// Where the configuration file lives: `$XDG_CONFIG_HOME/tessera/config.toml`,
/// or `~/.config/tessera/config.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Splits a command line into words, honouring single and double quotes.
///
/// Used for the configured terminal command and for `.desktop` Exec lines,
/// neither of which is run through a shell.
pub fn split_command(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut had_word = false;

    for ch in command.chars() {
        match (quote, ch) {
            (Some(open), ch) if ch == open => quote = None,
            (Some(_), ch) => word.push(ch),
            (None, '\'' | '"') => {
                quote = Some(ch);
                had_word = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if had_word || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    had_word = false;
                }
            }
            (None, ch) => word.push(ch),
        }
    }
    if had_word || !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_on_whitespace_and_respect_quotes() {
        assert_eq!(split_command("foot -e htop"), vec!["foot", "-e", "htop"]);
        assert_eq!(
            split_command(r#"sh -c "echo hello world""#),
            vec!["sh", "-c", "echo hello world"]
        );
        assert_eq!(
            split_command("env VAR='a b' prog"),
            vec!["env", "VAR=a b", "prog"]
        );
        assert_eq!(split_command(r#"prog "" x"#), vec!["prog", "", "x"]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn the_config_file_lives_under_the_config_directory() {
        let path = config_path();
        assert!(path.ends_with("tessera/config.toml"), "{path:?}");
    }
}
