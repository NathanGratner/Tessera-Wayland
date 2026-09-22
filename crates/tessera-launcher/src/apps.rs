//! The installed-application catalogue, read from `.desktop` files.

use freedesktop_desktop_entry::{DesktopEntry, Iter, default_paths, get_languages_from_env};
use nucleo_matcher::{
    Matcher,
    pattern::{CaseMatching, Normalization, Pattern},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub name: String,
    pub description: Option<String>,
    /// The command to run, with the `.desktop` field codes (%U, %f, …) removed.
    pub command: String,
    pub terminal: bool,
}

/// Every application worth showing, sorted by name.
#[derive(Debug, Default)]
pub struct Catalog {
    apps: Vec<App>,
}

impl Catalog {
    /// Reads `$XDG_DATA_DIRS/applications` and the user's own applications directory.
    pub fn load() -> Self {
        let locales = get_languages_from_env();
        let mut apps: Vec<App> = Iter::new(default_paths())
            .filter_map(|path| DesktopEntry::from_path(path, Some(&locales)).ok())
            .filter_map(|entry| Self::app_from(&entry, &locales))
            .collect();

        apps.sort_by_key(|app| app.name.to_lowercase());
        apps.dedup_by(|a, b| a.name == b.name && a.command == b.command);
        Self { apps }
    }

    fn app_from(entry: &DesktopEntry, locales: &[String]) -> Option<App> {
        if entry.no_display() || entry.type_() != Some("Application") {
            return None;
        }
        let name = entry.name(locales)?.to_string();
        let command = clean_exec(entry.exec()?);
        if command.is_empty() {
            return None;
        }
        Some(App {
            name,
            description: entry.comment(locales).map(|text| text.to_string()),
            command,
            terminal: entry.terminal(),
        })
    }

    /// Builds a catalogue from a known list, for tests.
    #[cfg(test)]
    pub fn from_apps(apps: Vec<App>) -> Self {
        Self { apps }
    }

    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// Fuzzy-matches `query` against application names, best match first.
    /// An empty query keeps the alphabetical order.
    pub fn search(&self, query: &str) -> Vec<&App> {
        if query.trim().is_empty() {
            return self.apps.iter().collect();
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

        let mut scored: Vec<(u32, &App)> = self
            .apps
            .iter()
            .filter_map(|app| {
                let haystack = nucleo_matcher::Utf32Str::Ascii(app.name.as_bytes());
                let mut buf = Vec::new();
                let haystack = if app.name.is_ascii() {
                    haystack
                } else {
                    nucleo_matcher::Utf32Str::new(&app.name, &mut buf)
                };
                pattern
                    .score(haystack, &mut matcher)
                    .map(|score| (score, app))
            })
            .collect();

        scored.sort_by(|(a_score, a), (b_score, b)| {
            b_score.cmp(a_score).then_with(|| a.name.cmp(&b.name))
        });
        scored.into_iter().map(|(_, app)| app).collect()
    }
}

/// Strips the `%f`, `%U`, … placeholders a `.desktop` Exec line may contain.
fn clean_exec(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|word| !(word.len() == 2 && word.starts_with('%')))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(names: &[(&str, &str)]) -> Catalog {
        Catalog {
            apps: names
                .iter()
                .map(|(name, command)| App {
                    name: name.to_string(),
                    description: None,
                    command: command.to_string(),
                    terminal: false,
                })
                .collect(),
        }
    }

    #[test]
    fn exec_placeholders_are_removed() {
        assert_eq!(clean_exec("firefox %u"), "firefox");
        assert_eq!(clean_exec("gimp-2.10 %U"), "gimp-2.10");
        assert_eq!(clean_exec("foot -e %F htop"), "foot -e htop");
        assert_eq!(clean_exec("code --new-window"), "code --new-window");
    }

    #[test]
    fn an_empty_query_keeps_every_app_in_order() {
        let catalog = catalog(&[("Ark", "ark"), ("Firefox", "firefox")]);
        let found = catalog.search("  ");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "Ark");
    }

    #[test]
    fn fuzzy_search_ranks_and_filters() {
        let catalog = catalog(&[
            ("Firefox", "firefox"),
            ("Files", "nautilus"),
            ("Calculator", "kcalc"),
        ]);
        let found = catalog.search("fir");
        assert_eq!(found.first().map(|app| app.name.as_str()), Some("Firefox"));
        assert!(!found.iter().any(|app| app.name == "Calculator"));
        assert!(catalog.search("zzzz").is_empty());
    }

    #[test]
    fn search_ignores_case() {
        let catalog = catalog(&[("Konsole", "konsole")]);
        assert_eq!(catalog.search("KON").len(), 1);
        assert_eq!(catalog.search("kon").len(), 1);
    }
}
