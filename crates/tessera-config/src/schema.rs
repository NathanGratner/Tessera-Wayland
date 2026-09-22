//! What settings exist, their types, defaults and relationships (design §6).
//!
//! The schema is data, like the kernel's Kconfig: the configuration menu is
//! generated from it, and values are validated against it. Adding a setting
//! means adding an `[[item]]` to `schema.toml` and reading it somewhere.

use std::{collections::HashMap, sync::OnceLock};

use serde::Deserialize;

use crate::expr::Expr;

/// The built-in schema, embedded at compile time.
const BUILTIN: &str = include_str!("schema.toml");

/// A setting's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// For `bool` options.
    Bool(bool),
    /// For `int` options.
    Int(i64),
    /// For `string` and `choice` options.
    Text(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(value) => write!(f, "{value}"),
            Value::Int(value) => write!(f, "{value}"),
            Value::Text(value) => write!(f, "{value}"),
        }
    }
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Apply {
    /// As soon as the configuration is saved.
    Live,
    /// The next time the affected program starts.
    Restart,
}

/// What kind of value an option holds, with its constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// On or off.
    Bool,
    /// A whole number within an inclusive range.
    Int {
        /// Smallest allowed value.
        min: i64,
        /// Largest allowed value.
        max: i64,
    },
    /// Free text, optionally constrained by a named pattern.
    Text {
        /// A named check, currently only `binding`.
        pattern: Option<Pattern>,
    },
    /// One of a fixed list.
    Choice {
        /// The allowed values, in menu order.
        choices: Vec<String>,
    },
}

/// Named shapes a string option must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    /// A key binding such as `Mod+Shift+Return`.
    Binding,
}

/// One setting.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionDef {
    /// `section.name`, which is also where it lives in config.toml.
    pub key: String,
    /// The text shown in the menu.
    pub prompt: String,
    /// Longer explanation, shown with `?`.
    pub help: String,
    /// Type and constraints.
    pub kind: Kind,
    /// Value used when config.toml does not set it.
    pub default: Value,
    /// When a change takes effect.
    pub apply: Apply,
    /// Shown only when this is true.
    pub depends_on: Option<Expr>,
    /// Bool options forced on while this one is on.
    pub select: Vec<String>,
}

/// A submenu.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuDef {
    /// Referenced by options' `parent`.
    pub id: String,
    /// The text shown in the menu.
    pub prompt: String,
    /// Longer explanation, shown with `?`.
    pub help: String,
    /// Shown only when this is true.
    pub depends_on: Option<Expr>,
}

/// An entry in a menu: either a submenu or an option.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// A submenu, by index into [`Schema::menus`].
    Menu(usize),
    /// An option, by index into [`Schema::options`].
    Option(usize),
}

/// Why a schema is unusable. Only ever seen by developers editing `schema.toml`.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The TOML itself is malformed or has unknown fields.
    #[error("schema.toml does not parse: {0}")]
    Parse(String),
    /// An item is inconsistent with itself or with the rest of the schema.
    #[error("schema item `{item}`: {reason}")]
    Invalid {
        /// The key or menu id of the offending item.
        item: String,
        /// What is wrong.
        reason: String,
    },
}

/// Every setting, arranged into menus.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    /// All submenus.
    pub menus: Vec<MenuDef>,
    /// All options.
    pub options: Vec<OptionDef>,
    /// Entries of each menu in file order; `None` is the top level.
    children: HashMap<Option<String>, Vec<Entry>>,
    by_key: HashMap<String, usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    item: Vec<RawItem>,
}

/// A default as written, before the option's kind says how to read it.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawDefault {
    Bool(bool),
    Int(i64),
    Text(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    kind: String,
    id: Option<String>,
    key: Option<String>,
    parent: Option<String>,
    prompt: String,
    #[serde(default)]
    help: String,
    default: Option<RawDefault>,
    apply: Option<Apply>,
    min: Option<i64>,
    max: Option<i64>,
    choices: Option<Vec<String>>,
    pattern: Option<Pattern>,
    depends_on: Option<String>,
    #[serde(default)]
    select: Vec<String>,
}

impl Schema {
    /// Tessera's own settings. Parsed once; a broken built-in schema is a bug,
    /// and the tests guarantee it parses.
    pub fn builtin() -> &'static Schema {
        static SCHEMA: OnceLock<Schema> = OnceLock::new();
        SCHEMA.get_or_init(|| {
            Schema::parse(BUILTIN).unwrap_or_else(|err| panic!("built-in schema is invalid: {err}"))
        })
    }

    /// Parses and validates a schema.
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        let raw: RawFile =
            toml_edit::de::from_str(text).map_err(|err| SchemaError::Parse(err.to_string()))?;

        let mut schema = Schema {
            menus: Vec::new(),
            options: Vec::new(),
            children: HashMap::new(),
            by_key: HashMap::new(),
        };

        for item in raw.item {
            let depends_on = item
                .depends_on
                .as_deref()
                .map(Expr::parse)
                .transpose()
                .map_err(|err| SchemaError::Invalid {
                    item: item.key.clone().or(item.id.clone()).unwrap_or_default(),
                    reason: err.to_string(),
                })?;

            if item.kind == "menu" {
                let id = item.id.ok_or_else(|| SchemaError::Invalid {
                    item: item.prompt.clone(),
                    reason: "a menu needs an `id`".into(),
                })?;
                schema
                    .children
                    .entry(item.parent.clone())
                    .or_default()
                    .push(Entry::Menu(schema.menus.len()));
                schema.menus.push(MenuDef {
                    id,
                    prompt: item.prompt,
                    help: item.help,
                    depends_on,
                });
                continue;
            }

            let key = item.key.clone().ok_or_else(|| SchemaError::Invalid {
                item: item.prompt.clone(),
                reason: "an option needs a `key`".into(),
            })?;
            let invalid = |reason: String| SchemaError::Invalid {
                item: key.clone(),
                reason,
            };

            if key.split('.').count() != 2 || key.starts_with('.') || key.ends_with('.') {
                return Err(invalid("keys are written `section.name`".into()));
            }
            if schema.by_key.contains_key(&key) {
                return Err(invalid("defined twice".into()));
            }

            let kind = match item.kind.as_str() {
                "bool" => Kind::Bool,
                "int" => {
                    let (Some(min), Some(max)) = (item.min, item.max) else {
                        return Err(invalid("an int needs `min` and `max`".into()));
                    };
                    if min > max {
                        return Err(invalid(format!("min {min} is above max {max}")));
                    }
                    Kind::Int { min, max }
                }
                "string" => Kind::Text {
                    pattern: item.pattern,
                },
                "choice" => {
                    let choices = item.choices.clone().unwrap_or_default();
                    if choices.is_empty() {
                        return Err(invalid("a choice needs `choices`".into()));
                    }
                    Kind::Choice { choices }
                }
                other => return Err(invalid(format!("unknown kind `{other}`"))),
            };

            let default = item
                .default
                .ok_or_else(|| invalid("an option needs a `default`".into()))
                .and_then(|raw| parse_default(raw, &kind).map_err(invalid))?;
            check(&kind, &default).map_err(invalid)?;

            let apply = item.apply.ok_or_else(|| {
                invalid("an option needs `apply = \"live\"` or `\"restart\"`".into())
            })?;

            if !item.select.is_empty() && kind != Kind::Bool {
                return Err(invalid("only bool options can `select`".into()));
            }

            schema.by_key.insert(key.clone(), schema.options.len());
            schema
                .children
                .entry(item.parent.clone())
                .or_default()
                .push(Entry::Option(schema.options.len()));
            schema.options.push(OptionDef {
                key,
                prompt: item.prompt,
                help: item.help,
                kind,
                default,
                apply,
                depends_on,
                select: item.select,
            });
        }

        schema.validate_references()?;
        Ok(schema)
    }

    /// Checks everything that refers to something else: parents, dependencies, selects.
    fn validate_references(&self) -> Result<(), SchemaError> {
        let menu_ids: Vec<&str> = self.menus.iter().map(|menu| menu.id.as_str()).collect();
        for parent in self.children.keys().flatten() {
            if !menu_ids.contains(&parent.as_str()) {
                return Err(SchemaError::Invalid {
                    item: parent.clone(),
                    reason: "used as a `parent` but no menu has that id".into(),
                });
            }
        }

        let bool_key = |key: &str| {
            self.option(key)
                .is_some_and(|option| option.kind == Kind::Bool)
        };

        let expressions = self
            .options
            .iter()
            .map(|option| (option.key.as_str(), &option.depends_on))
            .chain(
                self.menus
                    .iter()
                    .map(|menu| (menu.id.as_str(), &menu.depends_on)),
            );
        for (item, expr) in expressions {
            for key in expr.iter().flat_map(|expr| expr.keys()) {
                if !bool_key(key) {
                    return Err(SchemaError::Invalid {
                        item: item.to_string(),
                        reason: format!(
                            "`depends_on` mentions `{key}`, which is not a bool option"
                        ),
                    });
                }
            }
        }

        for option in &self.options {
            for target in &option.select {
                if !bool_key(target) {
                    return Err(SchemaError::Invalid {
                        item: option.key.clone(),
                        reason: format!("`select` names `{target}`, which is not a bool option"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Looks an option up by key.
    pub fn option(&self, key: &str) -> Option<&OptionDef> {
        self.by_key.get(key).map(|index| &self.options[*index])
    }

    /// Looks a menu up by id.
    pub fn menu(&self, id: &str) -> Option<&MenuDef> {
        self.menus.iter().find(|menu| menu.id == id)
    }

    /// The entries of a menu, in order. `None` is the top level.
    pub fn entries(&self, menu: Option<&str>) -> &[Entry] {
        self.children
            .get(&menu.map(str::to_string))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The menu an option sits in, if any.
    pub fn parent_of(&self, key: &str) -> Option<&str> {
        let index = *self.by_key.get(key)?;
        self.children.iter().find_map(|(parent, entries)| {
            entries
                .contains(&Entry::Option(index))
                .then_some(parent.as_deref())
                .flatten()
        })
    }
}

fn parse_default(raw: RawDefault, kind: &Kind) -> Result<Value, String> {
    match (kind, raw) {
        (Kind::Bool, RawDefault::Bool(value)) => Ok(Value::Bool(value)),
        (Kind::Int { .. }, RawDefault::Int(value)) => Ok(Value::Int(value)),
        (Kind::Text { .. } | Kind::Choice { .. }, RawDefault::Text(value)) => {
            Ok(Value::Text(value))
        }
        _ => Err("`default` has the wrong type for this kind of option".into()),
    }
}

/// Checks a value against an option's type and constraints.
///
/// The message names the constraint, so a person can fix the value without
/// looking anything up.
pub fn check(kind: &Kind, value: &Value) -> Result<(), String> {
    match (kind, value) {
        (Kind::Bool, Value::Bool(_)) => Ok(()),
        (Kind::Int { min, max }, Value::Int(number)) => {
            if (*min..=*max).contains(number) {
                Ok(())
            } else {
                Err(format!("{number} is outside the allowed range {min}–{max}"))
            }
        }
        (Kind::Text { pattern }, Value::Text(text)) => match pattern {
            None => Ok(()),
            Some(Pattern::Binding) => crate::binding::Binding::parse(text)
                .map(|_| ())
                .map_err(|err| err.to_string()),
        },
        (Kind::Choice { choices }, Value::Text(text)) => {
            if choices.contains(text) {
                Ok(())
            } else {
                Err(format!("`{text}` is not one of: {}", choices.join(", ")))
            }
        }
        (Kind::Bool, other) => Err(format!("expected on or off, got `{other}`")),
        (Kind::Int { .. }, other) => Err(format!("expected a whole number, got `{other}`")),
        (Kind::Text { .. } | Kind::Choice { .. }, other) => {
            Err(format!("expected text, got `{other}`"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_schema_parses_and_every_default_is_valid() {
        let schema = Schema::builtin();
        assert!(!schema.options.is_empty());
        for option in &schema.options {
            assert_eq!(
                check(&option.kind, &option.default),
                Ok(()),
                "default of {}",
                option.key
            );
        }
    }

    #[test]
    fn the_builtin_schema_has_the_menus_the_launcher_expects() {
        let schema = Schema::builtin();
        let top: Vec<&str> = schema
            .entries(None)
            .iter()
            .filter_map(|entry| match entry {
                Entry::Menu(index) => Some(schema.menus[*index].id.as_str()),
                Entry::Option(_) => None,
            })
            .collect();
        assert_eq!(
            top,
            [
                "general", "display", "bindings", "layout", "focus", "launcher"
            ]
        );
        assert_eq!(schema.parent_of("layout.gaps"), Some("layout"));
        assert_eq!(schema.option("layout.gaps").unwrap().default, Value::Int(8));
    }

    #[test]
    fn int_ranges_are_enforced_with_a_useful_message() {
        let kind = Kind::Int { min: 0, max: 64 };
        assert_eq!(check(&kind, &Value::Int(64)), Ok(()));
        let err = check(&kind, &Value::Int(999)).unwrap_err();
        assert!(err.contains("0–64"), "{err}");
    }

    #[test]
    fn choices_list_what_is_allowed() {
        let kind = Kind::Choice {
            choices: vec!["alt".into(), "super".into()],
        };
        let err = check(&kind, &Value::Text("hyper".into())).unwrap_err();
        assert!(err.contains("alt, super"), "{err}");
    }

    #[test]
    fn a_schema_with_a_dangling_reference_is_rejected() {
        let text = r#"
            [[item]]
            kind = "bool"
            key = "a.b"
            prompt = "B"
            default = true
            apply = "live"
            depends_on = "a.missing"
        "#;
        let err = Schema::parse(text).unwrap_err().to_string();
        assert!(err.contains("a.missing"), "{err}");
    }

    #[test]
    fn select_must_target_a_bool() {
        let text = r#"
            [[item]]
            kind = "bool"
            key = "a.on"
            prompt = "On"
            default = false
            apply = "live"
            select = ["a.size"]

            [[item]]
            kind = "int"
            key = "a.size"
            prompt = "Size"
            default = 1
            min = 0
            max = 9
            apply = "live"
        "#;
        let err = Schema::parse(text).unwrap_err().to_string();
        assert!(err.contains("not a bool"), "{err}");
    }

    #[test]
    fn defaults_must_satisfy_their_own_constraints() {
        let text = r#"
            [[item]]
            kind = "int"
            key = "a.size"
            prompt = "Size"
            default = 100
            min = 0
            max = 9
            apply = "live"
        "#;
        let err = Schema::parse(text).unwrap_err().to_string();
        assert!(err.contains("0–9"), "{err}");
    }

    #[test]
    fn unknown_fields_are_caught() {
        let text = r#"
            [[item]]
            kind = "bool"
            key = "a.b"
            prompt = "B"
            default = true
            apply = "live"
            defualt = false
        "#;
        assert!(Schema::parse(text).is_err());
    }
}
