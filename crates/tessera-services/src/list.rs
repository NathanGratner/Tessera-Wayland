//! `services.toml`: the services shown in the launcher.
//!
//! ```toml
//! [[service]]
//! unit = "sshd.service"
//! scope = "system"
//! ```
//!
//! Edited through `toml_edit`, like `config.toml`, so comments survive.

use std::{fs, io, path::Path, path::PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use crate::{Scope, Service};

/// The comment a new file starts with.
const PREAMBLE: &str = "\
# Services shown in Tessera's \"Scripts & services\" screen.
# `tessera-serv add|remove <unit>` and the launcher edit this file;
# editing it by hand is fine too.
";

/// The service a `[[service]]` table describes, if it is well formed.
fn listed(table: &Table) -> Option<Service> {
    let unit = table.get("unit").and_then(Item::as_str)?;
    let scope = table
        .get("scope")
        .and_then(Item::as_str)
        .and_then(Scope::parse)
        .unwrap_or(Scope::System);
    Service::new(unit, scope).ok()
}

/// Where the list lives: `$XDG_CONFIG_HOME/tessera/services.toml`.
pub fn services_path() -> PathBuf {
    tessera_config::config_dir().join("services.toml")
}

/// The tracked services, and the document they came from.
#[derive(Debug, Clone)]
pub struct ServiceList {
    document: DocumentMut,
}

impl Default for ServiceList {
    fn default() -> Self {
        Self {
            document: PREAMBLE.parse().expect("the preamble is valid TOML"),
        }
    }
}

impl ServiceList {
    /// Reads the file; a missing file is an empty list.
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|err| format!("{}: {err}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(format!("cannot read {}: {err}", path.display())),
        }
    }

    /// Parses the file's text, checking every entry.
    pub fn parse(text: &str) -> Result<Self, String> {
        let document: DocumentMut = text.parse().map_err(|err| format!("{err}"))?;
        let list = Self { document };
        list.try_services()?;
        Ok(list)
    }

    /// The services, in file order.
    pub fn services(&self) -> Vec<Service> {
        self.try_services().unwrap_or_default()
    }

    fn try_services(&self) -> Result<Vec<Service>, String> {
        let Some(item) = self.document.get("service") else {
            return Ok(Vec::new());
        };
        let tables = item
            .as_array_of_tables()
            .ok_or("`service` should be a list of [[service]] tables")?;
        tables
            .iter()
            .enumerate()
            .map(|(index, table)| {
                let entry = index + 1;
                let unit = table
                    .get("unit")
                    .and_then(Item::as_str)
                    .ok_or(format!("service {entry} has no `unit`"))?;
                let scope = match table.get("scope").and_then(Item::as_str) {
                    None => Scope::System,
                    Some(text) => Scope::parse(text).ok_or(format!(
                        "service {entry}: scope `{text}` is not system or user"
                    ))?,
                };
                Service::new(unit, scope).map_err(|err| format!("service {entry}: {err}"))
            })
            .collect()
    }

    /// Adds a service; false if it was already listed.
    pub fn add(&mut self, service: &Service) -> bool {
        if self.services().contains(service) {
            return false;
        }
        let mut table = Table::new();
        table.insert("unit", toml_edit::value(service.unit.as_str()));
        table.insert("scope", toml_edit::value(service.scope.as_str()));
        match self
            .document
            .get_mut("service")
            .and_then(Item::as_array_of_tables_mut)
        {
            Some(tables) => tables.push(table),
            None => {
                // Comments in a file with no entries yet are the document's
                // trailing text, which would end up *after* the new table.
                // Moving them in front of it keeps the preamble on top.
                let trailing = self
                    .document
                    .trailing()
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                self.document.set_trailing("");
                // A blank line between the preamble and the first entry.
                let prefix = if trailing.trim().is_empty() {
                    trailing
                } else {
                    format!("{}\n\n", trailing.trim_end())
                };
                table.decor_mut().set_prefix(prefix);
                let mut tables = ArrayOfTables::new();
                tables.push(table);
                self.document.insert("service", Item::ArrayOfTables(tables));
            }
        }
        true
    }

    /// Removes a service; false if it was not listed.
    pub fn remove(&mut self, service: &Service) -> bool {
        let Some(tables) = self
            .document
            .get_mut("service")
            .and_then(Item::as_array_of_tables_mut)
        else {
            return false;
        };
        let Some(index) = tables
            .iter()
            .position(|table| listed(table) == Some(service.clone()))
        else {
            return false;
        };
        // A comment written above this entry belongs to the file, not the
        // entry, so it moves to whatever comes next rather than vanishing.
        let prefix = tables
            .get(index)
            .and_then(|table| table.decor().prefix())
            .and_then(|raw| raw.as_str())
            .filter(|text| text.contains('#'))
            .map(str::to_string);
        tables.remove(index);
        if let Some(prefix) = prefix {
            match tables.get_mut(index) {
                Some(next) => {
                    let rest = next
                        .decor()
                        .prefix()
                        .and_then(|raw| raw.as_str())
                        .unwrap_or_default()
                        .to_string();
                    next.decor_mut().set_prefix(format!("{prefix}{rest}"));
                }
                None => {
                    let rest = self.document.trailing().as_str().unwrap_or_default();
                    let trailing = format!("{prefix}{rest}");
                    self.document.set_trailing(trailing);
                }
            }
        }
        true
    }

    /// Writes the list atomically: a temporary file, then a rename.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let fail = |err: io::Error| format!("cannot write {}: {err}", path.display());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(fail)?;
        }
        let temp = path.with_extension("toml.tmp");
        fs::write(&temp, self.document.to_string()).map_err(fail)?;
        fs::rename(&temp, path).map_err(fail)
    }

    /// The file's text, as it would be saved.
    pub fn to_toml(&self) -> String {
        self.document.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(unit: &str, scope: Scope) -> Service {
        Service::new(unit, scope).unwrap()
    }

    #[test]
    fn an_empty_list_gains_entries_and_explains_itself() {
        let mut list = ServiceList::default();
        assert!(list.add(&service("sshd", Scope::System)));
        assert!(list.add(&service("syncthing", Scope::User)));
        assert!(
            !list.add(&service("sshd.service", Scope::System)),
            "no duplicates"
        );

        let text = list.to_toml();
        assert!(text.starts_with("# Services shown"), "{text}");
        assert!(text.contains("fine too.\n\n[[service]]"), "{text}");
        let again = ServiceList::parse(&text).unwrap();
        assert_eq!(
            again.services(),
            [
                service("sshd", Scope::System),
                service("syncthing", Scope::User)
            ]
        );
    }

    #[test]
    fn the_same_unit_in_both_scopes_is_two_entries() {
        let mut list = ServiceList::default();
        assert!(list.add(&service("foo", Scope::System)));
        assert!(list.add(&service("foo", Scope::User)));
        assert_eq!(list.services().len(), 2);
    }

    #[test]
    fn removing_keeps_comments_and_other_entries() {
        let mut list = ServiceList::parse(
            "# mine\n[[service]]\nunit = \"a.service\"\n\n[[service]]\nunit = \"b\"\nscope = \"user\"\n",
        )
        .unwrap();
        assert!(list.remove(&service("a", Scope::System)));
        assert!(!list.remove(&service("a", Scope::System)));
        let text = list.to_toml();
        assert!(text.contains("# mine"), "{text}");
        assert_eq!(list.services(), [service("b", Scope::User)]);
    }

    #[test]
    fn scope_defaults_to_system() {
        let list = ServiceList::parse("[[service]]\nunit = \"sshd\"\n").unwrap();
        assert_eq!(list.services(), [service("sshd", Scope::System)]);
    }

    #[test]
    fn mistakes_in_the_file_are_named() {
        let err = ServiceList::parse("[[service]]\nscope = \"user\"\n").unwrap_err();
        assert!(err.contains("no `unit`"), "{err}");
        let err = ServiceList::parse("[[service]]\nunit = \"x\"\nscope = \"root\"\n").unwrap_err();
        assert!(err.contains("root"), "{err}");
        let err = ServiceList::parse("service = 3\n").unwrap_err();
        assert!(err.contains("[[service]]"), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        let list = ServiceList::load(Path::new("/nonexistent/tessera/services.toml")).unwrap();
        assert!(list.services().is_empty());
    }
}
