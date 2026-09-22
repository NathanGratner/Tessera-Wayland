//! Finding scripts in the scripts folder.
//!
//! Pure file-system work with no compositor state, so it is tested against a
//! temporary folder.

use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use tessera_config::scripts::Header;

/// Headers live in the first 20 lines; this many bytes always covers them.
const READ_LIMIT: u64 = 16 * 1024;

/// A script found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// The file name, which identifies the script.
    pub name: String,
    /// Full path.
    pub path: PathBuf,
    /// What its header says.
    pub header: Header,
}

impl Discovered {
    /// The name shown to people: the header's, or the file name.
    pub fn title(&self) -> &str {
        self.header.name.as_deref().unwrap_or(&self.name)
    }
}

/// Every executable file in `dir`, sorted by title.
///
/// Hidden files and editor leftovers (`name~`, `.swp`) are skipped, so saving
/// a script in an editor does not briefly add a second one. A missing folder
/// simply has no scripts.
pub fn scan(dir: &Path) -> Vec<Discovered> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Discovered> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if is_ignored(&name) {
                return None;
            }
            let path = entry.path();
            // `metadata` follows symlinks, so a link to a script counts.
            let metadata = fs::metadata(&path).ok()?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
            let header = Header::parse(&read_head(&path));
            Some(Discovered { name, path, header })
        })
        .collect();
    found.sort_by(|a, b| {
        a.title()
            .to_lowercase()
            .cmp(&b.title().to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    found
}

/// Files that are never scripts even when executable.
fn is_ignored(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with('~')
        || [".swp", ".swo", ".bak", ".orig", ".tmp"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// The start of a file, as text. Binary executables yield no header, which is fine.
fn read_head(path: &Path) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = fs::File::open(path) {
        let _ = file.take(READ_LIMIT).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, removed afterwards.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("tessera-scripts-{tag}-{}", std::process::id()));
            fs::remove_dir_all(&path).ok();
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, text: &str, mode: u32) {
            let path = self.0.join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn scripts_are_executables_sorted_by_title() {
        let dir = TempDir::new("sorted");
        dir.write("zeta", "#!/bin/sh\n# tessera: name = \"Alpha\"\n", 0o755);
        dir.write("beta", "#!/bin/sh\n", 0o700);
        dir.write("notes.txt", "not a script", 0o644);

        let found = scan(&dir.0);
        let names: Vec<&str> = found.iter().map(|script| script.name.as_str()).collect();
        assert_eq!(names, ["zeta", "beta"], "Alpha sorts before beta");
        assert_eq!(found[0].title(), "Alpha");
        assert_eq!(found[1].title(), "beta");
    }

    #[test]
    fn hidden_files_and_editor_leftovers_are_skipped() {
        let dir = TempDir::new("skipped");
        for name in [".hidden", "backup~", "script.swp", "old.bak"] {
            dir.write(name, "#!/bin/sh\n", 0o755);
        }
        dir.write("real", "#!/bin/sh\n", 0o755);
        let found = scan(&dir.0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "real");
    }

    #[test]
    fn folders_are_not_scripts() {
        let dir = TempDir::new("folders");
        fs::create_dir(dir.0.join("lib")).unwrap();
        assert!(scan(&dir.0).is_empty());
    }

    #[test]
    fn a_missing_folder_has_no_scripts() {
        assert!(scan(Path::new("/nonexistent/tessera/scripts")).is_empty());
    }

    #[test]
    fn header_problems_travel_with_the_script() {
        let dir = TempDir::new("problems");
        dir.write(
            "broken",
            "#!/bin/sh\n# tessera: mode = \"sideways\"\n",
            0o755,
        );
        let found = scan(&dir.0);
        assert_eq!(found[0].header.problems.len(), 1);
    }
}
