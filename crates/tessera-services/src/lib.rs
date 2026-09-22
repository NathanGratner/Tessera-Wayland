//! systemd services in Tessera: which ones to show, and how to drive them.
//!
//! systemd already supervises services, so Tessera keeps only a *list* of the
//! units a person cares about (`services.toml`) and runs `systemctl` on their
//! behalf. The launcher's Scripts & services screen and the `tessera-serv`
//! command both use this crate.
//!
//! ```
//! use tessera_services::{Scope, Service};
//!
//! let service = Service::new("sshd", Scope::System).unwrap();
//! assert_eq!(service.unit, "sshd.service");
//! ```

#![warn(missing_docs)]

pub mod list;
pub mod systemctl;

pub use list::{ServiceList, services_path};
pub use systemctl::{ActionError, PowerAction, UnitState, Verb};

use std::fmt;

/// Which service manager a unit belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// The system manager; changing these usually needs a password.
    System,
    /// The user's own manager (`systemctl --user`); never needs one.
    User,
}

impl Scope {
    /// The name used in `services.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::System => "system",
            Scope::User => "user",
        }
    }

    /// Parses the name used in `services.toml`.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "system" => Some(Scope::System),
            "user" => Some(Scope::User),
            _ => None,
        }
    }
}

/// A unit and the manager it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Service {
    /// Full unit name, e.g. `sshd.service`.
    pub unit: String,
    /// System or user manager.
    pub scope: Scope,
}

impl Service {
    /// A service by name; `sshd` becomes `sshd.service`, as systemctl would read it.
    pub fn new(name: &str, scope: Scope) -> Result<Self, String> {
        Ok(Self {
            unit: normalize_unit(name)?,
            scope,
        })
    }
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scope {
            Scope::System => write!(f, "{}", self.unit),
            Scope::User => write!(f, "{} (user)", self.unit),
        }
    }
}

/// Unit types systemctl accepts by suffix; anything else gets `.service`.
const UNIT_SUFFIXES: [&str; 11] = [
    ".service",
    ".socket",
    ".timer",
    ".target",
    ".path",
    ".mount",
    ".automount",
    ".swap",
    ".device",
    ".slice",
    ".scope",
];

/// Checks a unit name and adds `.service` when no unit type is given, so
/// `sshd` and `sshd.service` are the same entry.
pub fn normalize_unit(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a unit name is needed, e.g. sshd.service".into());
    }
    if name.starts_with('-') {
        return Err(format!("`{name}` looks like an option, not a unit name"));
    }
    if name.contains(['/', ' ', '\t', '\n']) {
        return Err(format!("`{name}` is not a unit name"));
    }
    if UNIT_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
        Ok(name.to_string())
    } else {
        Ok(format!("{name}.service"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_gain_service_only_when_untyped() {
        assert_eq!(normalize_unit("sshd").unwrap(), "sshd.service");
        assert_eq!(normalize_unit(" sshd.service ").unwrap(), "sshd.service");
        assert_eq!(normalize_unit("fstrim.timer").unwrap(), "fstrim.timer");
        assert_eq!(normalize_unit("getty@tty2").unwrap(), "getty@tty2.service");
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(normalize_unit("").is_err());
        assert!(normalize_unit("--now").is_err());
        assert!(normalize_unit("../../etc").is_err());
        assert!(normalize_unit("two words").is_err());
    }

    #[test]
    fn user_services_say_so() {
        let service = Service::new("syncthing", Scope::User).unwrap();
        assert_eq!(service.to_string(), "syncthing.service (user)");
        assert_eq!(Scope::parse(Scope::User.as_str()), Some(Scope::User));
    }
}
