//! Reading, editing and saving `config.toml` against the schema.
//!
//! Edits go through a `toml_edit` document rather than a serialised struct, so
//! saving keeps the user's comments, ordering and any keys we do not know about.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use toml_edit::{DocumentMut, Item, Table};

use crate::schema::{Apply, Kind, Schema, Value, check};

/// A setting that could not be accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{key} = {value}: {reason}")]
pub struct ConfigError {
    /// The option's key, e.g. `layout.gaps`.
    pub key: String,
    /// The value as written.
    pub value: String,
    /// Why it was refused, naming the constraint.
    pub reason: String,
}

/// Why a configuration file could not be loaded or saved.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Reading or writing the file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file involved.
        path: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not valid TOML.
    #[error("{path} is not valid TOML: {message}")]
    Syntax {
        /// The file involved.
        path: String,
        /// What the parser said.
        message: String,
    },
    /// A value breaks the schema.
    #[error(transparent)]
    Invalid(#[from] ConfigError),
}

/// One changed setting, as reported after a reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The option that changed.
    pub key: String,
    /// When the change takes effect.
    pub apply: Apply,
}

/// Configuration values, backed by an editable TOML document.
#[derive(Debug, Clone)]
pub struct ConfigValues {
    schema: &'static Schema,
    document: DocumentMut,
}

impl Default for ConfigValues {
    fn default() -> Self {
        Self {
            schema: Schema::builtin(),
            document: DocumentMut::new(),
        }
    }
}

impl ConfigValues {
    /// Loads a file. A missing file is not an error: every option takes its default.
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|err| match err {
                LoadError::Syntax { message, .. } => LoadError::Syntax {
                    path: path.display().to_string(),
                    message,
                },
                other => other,
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LoadError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Parses configuration text and checks every known key against the schema.
    ///
    /// Keys the schema does not know are left alone rather than rejected, so an
    /// older Tessera does not choke on a newer config.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        Self::parse_with(Schema::builtin(), text)
    }

    /// Like [`ConfigValues::parse`], against a schema other than the built-in one.
    pub fn parse_with(schema: &'static Schema, text: &str) -> Result<Self, LoadError> {
        let document: DocumentMut =
            text.parse()
                .map_err(|err: toml_edit::TomlError| LoadError::Syntax {
                    path: "config".into(),
                    message: err.to_string(),
                })?;
        let values = Self { schema, document };
        for option in &values.schema.options {
            if let Some(item) = values.raw(&option.key) {
                let value = read_item(item, &option.kind).ok_or_else(|| ConfigError {
                    key: option.key.clone(),
                    value: item.to_string().trim().to_string(),
                    reason: match option.kind {
                        Kind::Bool => "expected true or false".into(),
                        Kind::Int { .. } => "expected a whole number".into(),
                        _ => "expected text in quotes".into(),
                    },
                })?;
                check(&option.kind, &value).map_err(|reason| ConfigError {
                    key: option.key.clone(),
                    value: value.to_string(),
                    reason,
                })?;
            }
        }
        Ok(values)
    }

    /// The schema these values follow.
    pub fn schema(&self) -> &'static Schema {
        self.schema
    }

    fn raw(&self, key: &str) -> Option<&Item> {
        let (section, name) = key.split_once('.')?;
        self.document.get(section)?.get(name)
    }

    /// The value as stored: the file's value, or the default.
    pub fn stored(&self, key: &str) -> Option<Value> {
        let option = self.schema.option(key)?;
        Some(
            self.raw(key)
                .and_then(|item| read_item(item, &option.kind))
                .unwrap_or_else(|| option.default.clone()),
        )
    }

    /// The value in effect: the stored value, unless another option `select`s it on.
    pub fn get(&self, key: &str) -> Option<Value> {
        if self.forced_by(key).is_some() {
            return Some(Value::Bool(true));
        }
        self.stored(key)
    }

    /// The option currently forcing `key` on, if any.
    pub fn forced_by(&self, key: &str) -> Option<&str> {
        self.schema
            .options
            .iter()
            .find(|option| {
                option.select.iter().any(|target| target == key)
                    && self.stored(&option.key) == Some(Value::Bool(true))
            })
            .map(|option| option.key.as_str())
    }

    /// Convenience accessor for bool options; false for unknown keys.
    pub fn bool(&self, key: &str) -> bool {
        matches!(self.get(key), Some(Value::Bool(true)))
    }

    /// Convenience accessor for int options; 0 for unknown keys.
    pub fn int(&self, key: &str) -> i64 {
        match self.get(key) {
            Some(Value::Int(value)) => value,
            _ => 0,
        }
    }

    /// Convenience accessor for string and choice options; empty for unknown keys.
    pub fn text(&self, key: &str) -> String {
        match self.get(key) {
            Some(Value::Text(value)) => value,
            _ => String::new(),
        }
    }

    /// Whether an option's `depends_on` is satisfied, so it should be shown and honoured.
    pub fn visible(&self, key: &str) -> bool {
        self.schema
            .option(key)
            .and_then(|option| option.depends_on.as_ref())
            .is_none_or(|expr| expr.eval(&|name: &str| self.bool(name)))
    }

    /// Whether a menu's `depends_on` is satisfied.
    pub fn menu_visible(&self, id: &str) -> bool {
        self.schema
            .menu(id)
            .and_then(|menu| menu.depends_on.as_ref())
            .is_none_or(|expr| expr.eval(&|name: &str| self.bool(name)))
    }

    /// Changes a value, refusing anything the schema does not allow.
    ///
    /// Setting a value back to its default removes it from the file, so the
    /// file only records what the user actually changed.
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), ConfigError> {
        let Some(option) = self.schema.option(key) else {
            return Err(ConfigError {
                key: key.to_string(),
                value: value.to_string(),
                reason: "there is no such setting".into(),
            });
        };
        check(&option.kind, &value).map_err(|reason| ConfigError {
            key: key.to_string(),
            value: value.to_string(),
            reason,
        })?;

        let (section, name) = key.split_once('.').expect("schema keys are section.name");
        if value == option.default {
            if let Some(table) = self.document.get_mut(section).and_then(Item::as_table_mut) {
                table.remove(name);
                // Drop a section left empty, unless the user commented on it.
                if table.is_empty() && !has_comment(table.decor()) {
                    self.document.remove(section);
                }
            }
            return Ok(());
        }

        if !self.document.contains_table(section) {
            self.document.insert(section, Item::Table(Table::new()));
        }
        let item = match value {
            Value::Bool(value) => toml_edit::value(value),
            Value::Int(value) => toml_edit::value(value),
            Value::Text(value) => toml_edit::value(value),
        };
        self.document[section][name] = item;
        Ok(())
    }

    /// Parses and sets a value typed by the user into an input box.
    pub fn set_from_text(&mut self, key: &str, text: &str) -> Result<(), ConfigError> {
        let Some(option) = self.schema.option(key) else {
            return Err(ConfigError {
                key: key.to_string(),
                value: text.to_string(),
                reason: "there is no such setting".into(),
            });
        };
        let value = match option.kind {
            Kind::Bool => match text.trim() {
                "true" | "y" | "yes" | "on" => Value::Bool(true),
                "false" | "n" | "no" | "off" => Value::Bool(false),
                _ => {
                    return Err(ConfigError {
                        key: key.to_string(),
                        value: text.to_string(),
                        reason: "expected y or n".into(),
                    });
                }
            },
            Kind::Int { .. } => Value::Int(text.trim().parse().map_err(|_| ConfigError {
                key: key.to_string(),
                value: text.to_string(),
                reason: "expected a whole number".into(),
            })?),
            Kind::Text { .. } | Kind::Choice { .. } => Value::Text(text.to_string()),
        };
        self.set(key, value)
    }

    /// The configuration as TOML, exactly as it would be saved.
    pub fn to_toml(&self) -> String {
        self.document.to_string()
    }

    /// Saves atomically: write a temporary file beside the target, then rename
    /// it over the original, so a crash mid-save never leaves a truncated config.
    pub fn save(&self, path: &Path) -> Result<(), LoadError> {
        let io_error = |source| LoadError::Io {
            path: path.display().to_string(),
            source,
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_error)?;
        }
        let temporary = path.with_extension("toml.tmp");
        {
            let mut file = fs::File::create(&temporary).map_err(io_error)?;
            file.write_all(self.to_toml().as_bytes())
                .map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
        }
        fs::rename(&temporary, path).map_err(io_error)
    }

    /// Every option whose effective value differs between two configurations.
    pub fn changes_from(&self, old: &ConfigValues) -> Vec<Change> {
        self.schema
            .options
            .iter()
            .filter(|option| self.get(&option.key) != old.get(&option.key))
            .map(|option| Change {
                key: option.key.clone(),
                apply: option.apply,
            })
            .collect()
    }
}

/// Reads a TOML item as the given kind, or `None` when the type is wrong.
/// Whether a table's surrounding whitespace holds a comment worth keeping.
fn has_comment(decor: &toml_edit::Decor) -> bool {
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .any(|raw| raw.as_str().is_some_and(|text| text.contains('#')))
}

fn read_item(item: &Item, kind: &Kind) -> Option<Value> {
    match kind {
        Kind::Bool => item.as_bool().map(Value::Bool),
        Kind::Int { .. } => item.as_integer().map(Value::Int),
        Kind::Text { .. } | Kind::Choice { .. } => {
            item.as_str().map(|text| Value::Text(text.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_uses_every_default() {
        let values = ConfigValues::parse("").unwrap();
        assert_eq!(values.int("layout.gaps"), 8);
        assert_eq!(values.text("general.terminal"), "foot");
        assert!(!values.bool("layout.snap_to_cells"));
        assert!(values.bool("focus.new_windows"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let values = ConfigValues::load(Path::new("/nonexistent/tessera/config.toml")).unwrap();
        assert_eq!(values.int("layout.gaps"), 8);
    }

    #[test]
    fn file_values_override_defaults() {
        let values = ConfigValues::parse("[layout]\ngaps = 24\n").unwrap();
        assert_eq!(values.int("layout.gaps"), 24);
    }

    #[test]
    fn out_of_range_values_are_rejected_naming_the_range() {
        let err = ConfigValues::parse("[layout]\ngaps = 999\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("layout.gaps"), "{err}");
        assert!(err.contains("999"), "{err}");
        assert!(err.contains("0–64"), "{err}");

        let mut values = ConfigValues::default();
        let err = values.set("layout.gaps", Value::Int(999)).unwrap_err();
        assert!(err.reason.contains("0–64"), "{err}");
    }

    #[test]
    fn wrong_types_are_rejected() {
        let err = ConfigValues::parse("[layout]\ngaps = \"wide\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn unknown_keys_are_kept_rather_than_rejected() {
        let values = ConfigValues::parse("[future]\nshiny = true\n").unwrap();
        assert!(values.to_toml().contains("shiny = true"));
    }

    #[test]
    fn saving_keeps_comments_and_unknown_keys() {
        let text = "# my settings\n[layout]\n# roomy\ngaps = 12\n\n[future]\nshiny = true\n";
        let mut values = ConfigValues::parse(text).unwrap();
        values.set("layout.gaps", Value::Int(24)).unwrap();
        let saved = values.to_toml();
        assert!(saved.contains("# my settings"), "{saved}");
        assert!(saved.contains("# roomy"), "{saved}");
        assert!(saved.contains("gaps = 24"), "{saved}");
        assert!(saved.contains("shiny = true"), "{saved}");
    }

    #[test]
    fn setting_a_default_removes_the_line() {
        let mut values = ConfigValues::parse("[layout]\ngaps = 12\n").unwrap();
        values.set("layout.gaps", Value::Int(8)).unwrap();
        assert!(!values.to_toml().contains("gaps"), "{}", values.to_toml());
        assert_eq!(values.int("layout.gaps"), 8);
    }

    #[test]
    fn setting_the_last_default_drops_the_empty_section() {
        let mut values = ConfigValues::parse("[layout]\ngaps = 12\n").unwrap();
        values.set("layout.gaps", Value::Int(8)).unwrap();
        assert!(
            !values.to_toml().contains("[layout]"),
            "{}",
            values.to_toml()
        );
    }

    #[test]
    fn an_empty_section_with_a_comment_is_kept() {
        let mut values = ConfigValues::parse("# spacing\n[layout]\ngaps = 12\n").unwrap();
        values.set("layout.gaps", Value::Int(8)).unwrap();
        assert!(
            values.to_toml().contains("# spacing"),
            "{}",
            values.to_toml()
        );
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("tessera-config-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.toml");
        let mut values = ConfigValues::default();
        values
            .set("general.terminal", Value::Text("alacritty".into()))
            .unwrap();
        values.save(&path).unwrap();

        assert!(
            !path.with_extension("toml.tmp").exists(),
            "temp file is renamed away"
        );
        let loaded = ConfigValues::load(&path).unwrap();
        assert_eq!(loaded.text("general.terminal"), "alacritty");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn depends_on_hides_options_until_satisfied() {
        let mut values = ConfigValues::default();
        assert!(!values.visible("focus.follows_mouse_skips_launcher"));
        values
            .set("focus.follows_mouse", Value::Bool(true))
            .unwrap();
        assert!(values.visible("focus.follows_mouse_skips_launcher"));
        assert!(
            values.visible("layout.gaps"),
            "options without depends_on are always visible"
        );
    }

    #[test]
    fn changes_report_how_each_takes_effect() {
        let old = ConfigValues::default();
        let mut new = old.clone();
        new.set("layout.gaps", Value::Int(20)).unwrap();
        new.set("launcher.font_size", Value::Int(20)).unwrap();
        let changes = new.changes_from(&old);
        assert_eq!(
            changes,
            vec![
                Change {
                    key: "layout.gaps".into(),
                    apply: Apply::Live
                },
                Change {
                    key: "launcher.font_size".into(),
                    apply: Apply::Restart
                },
            ]
        );
    }

    #[test]
    fn select_forces_an_option_on_while_its_selector_is_on() {
        // The built-in schema has no `select`, so exercise the mechanism on its own.
        let schema: &'static Schema = Box::leak(Box::new(
            Schema::parse(
                r#"
                [[item]]
                kind = "bool"
                key = "net.vpn"
                prompt = "VPN"
                default = false
                apply = "live"
                select = ["net.firewall"]

                [[item]]
                kind = "bool"
                key = "net.firewall"
                prompt = "Firewall"
                default = false
                apply = "live"
                "#,
            )
            .unwrap(),
        ));

        let mut values = ConfigValues::parse_with(schema, "").unwrap();
        assert!(!values.bool("net.firewall"));
        assert_eq!(values.forced_by("net.firewall"), None);

        values.set("net.vpn", Value::Bool(true)).unwrap();
        assert!(values.bool("net.firewall"), "selected on");
        assert_eq!(values.forced_by("net.firewall"), Some("net.vpn"));
        assert_eq!(
            values.stored("net.firewall"),
            Some(Value::Bool(false)),
            "the stored value is untouched, so it comes back when the selector is off"
        );

        values.set("net.vpn", Value::Bool(false)).unwrap();
        assert!(!values.bool("net.firewall"));
    }

    #[test]
    fn text_input_is_parsed_per_kind() {
        let mut values = ConfigValues::default();
        values.set_from_text("layout.gaps", " 16 ").unwrap();
        assert_eq!(values.int("layout.gaps"), 16);
        assert!(values.set_from_text("layout.gaps", "lots").is_err());
        let err = values
            .set_from_text("bindings.quit", "Hyper+q")
            .unwrap_err();
        assert!(err.reason.contains("not a modifier"), "{err}");
    }
}
