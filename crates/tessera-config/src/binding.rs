//! Key bindings written as text: `Mod+Shift+Return`.
//!
//! This only checks the *shape*. Whether `Return` names a real key is decided
//! by the compositor, which has xkb; it reports unknown key names when the
//! configuration is applied.

use std::fmt;

/// A parsed binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Held with the configured modifier key.
    pub mod_key: bool,
    /// Held with Shift.
    pub shift: bool,
    /// Held with Ctrl.
    pub ctrl: bool,
    /// The xkb key name, e.g. `Return`, `backslash`, `e`.
    pub key: String,
}

/// Why a binding string is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{text}` is not a binding: {reason}. Write it like Mod+Shift+Return")]
pub struct BindingError {
    /// The string as written.
    pub text: String,
    /// What is wrong with it.
    pub reason: String,
}

impl Binding {
    /// Parses `Mod+Shift+Return`. Modifiers are case-insensitive and may come
    /// in any order; the key comes last. Every binding must use `Mod`, so
    /// Tessera never steals a key from the focused program.
    pub fn parse(text: &str) -> Result<Self, BindingError> {
        let error = |reason: &str| BindingError {
            text: text.to_string(),
            reason: reason.to_string(),
        };

        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, modifiers)) = parts.split_last() else {
            return Err(error("it is empty"));
        };
        if key.is_empty() {
            return Err(error("it has no key after the last +"));
        }

        let mut binding = Binding {
            mod_key: false,
            shift: false,
            ctrl: false,
            key: key.to_string(),
        };
        for modifier in modifiers {
            let slot = match modifier.to_ascii_lowercase().as_str() {
                "mod" => &mut binding.mod_key,
                "shift" => &mut binding.shift,
                "ctrl" | "control" => &mut binding.ctrl,
                "" => return Err(error("it has an empty part between two +")),
                other => {
                    return Err(error(&format!(
                        "`{other}` is not a modifier; use Mod, Shift or Ctrl"
                    )));
                }
            };
            if *slot {
                return Err(error(&format!("`{modifier}` appears twice")));
            }
            *slot = true;
        }
        if !binding.mod_key {
            return Err(error("every binding needs Mod"));
        }
        Ok(binding)
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mod")?;
        if self.ctrl {
            write!(f, "+Ctrl")?;
        }
        if self.shift {
            write!(f, "+Shift")?;
        }
        write!(f, "+{}", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let binding = Binding::parse("shift+MOD+Return").unwrap();
        assert!(binding.mod_key && binding.shift && !binding.ctrl);
        assert_eq!(binding.key, "Return");
        assert_eq!(binding.to_string(), "Mod+Shift+Return");
        assert!(Binding::parse("Mod+Control+q").unwrap().ctrl);
    }

    #[test]
    fn rejects_malformed_bindings_with_reasons() {
        for (text, fragment) in [
            ("Return", "needs Mod"),
            ("Mod+", "no key"),
            ("Mod++q", "empty part"),
            ("Mod+Hyper+q", "not a modifier"),
            ("Mod+Mod+q", "twice"),
        ] {
            let err = Binding::parse(text).unwrap_err().to_string();
            assert!(err.contains(fragment), "`{text}` → `{err}`");
            assert!(err.contains("Mod+Shift+Return"), "errors show an example");
        }
    }
}
