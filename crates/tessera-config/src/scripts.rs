//! User scripts: where they live, and the header that describes them (design §7).
//!
//! A script is any executable file in the scripts folder. An optional header
//! in its first [`HEADER_LINES`] lines gives it a name, a key binding and a
//! run mode, as comments, so the script still runs fine without Tessera:
//!
//! ```text
//! #!/bin/sh
//! # tessera: name = "Sync notes"
//! # tessera: description = "Push ~/notes to the remote and pull changes"
//! # tessera: mode = "background"        # background | terminal
//! # tessera: bind = "Mod+Shift+N"
//! # tessera: autostart = false
//! ```
//!
//! The compositor reads headers; the launcher rewrites the `autostart` line.
//! Both use this module, so they can never disagree about the format.

use std::path::PathBuf;

use crate::Binding;

/// Only this many lines at the top of a script are searched for a header.
pub const HEADER_LINES: usize = 20;

/// The marker that starts a header line, after the comment character.
const MARKER: &str = "tessera:";

/// The scripts folder: `$XDG_CONFIG_HOME/tessera/scripts`.
pub fn scripts_dir() -> PathBuf {
    crate::config_dir().join("scripts")
}

/// How a script runs when nothing says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// In the background, with its output captured by the compositor.
    #[default]
    Background,
    /// In a terminal tiled beside the launcher, showing its own output.
    Terminal,
}

/// What a script's header says about it. Every field is optional in the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    /// Display name; the file name is used when absent.
    pub name: Option<String>,
    /// One line saying what the script does.
    pub description: Option<String>,
    /// How it runs by default.
    pub mode: Mode,
    /// Key binding, already checked for shape (`Mod+Shift+N`).
    pub bind: Option<String>,
    /// Run once when the compositor starts.
    pub autostart: bool,
    /// Lines that looked like header lines but could not be understood, each
    /// explained. The rest of the header still applies.
    pub problems: Vec<String>,
}

impl Header {
    /// Reads the header from a script's text.
    ///
    /// Never fails: a malformed line is recorded in [`Header::problems`] and
    /// skipped, so one typo does not hide a script.
    pub fn parse(text: &str) -> Self {
        let mut header = Header::default();
        for (index, line) in text.lines().take(HEADER_LINES).enumerate() {
            let Some(body) = header_body(line) else {
                continue;
            };
            let line_no = index + 1;
            let problem = |reason: String| format!("line {line_no}: {reason}");

            let (key, value) = match parse_assignment(body) {
                Ok(pair) => pair,
                Err(reason) => {
                    header.problems.push(problem(reason));
                    continue;
                }
            };
            let text = |value: &HeaderValue| match value {
                HeaderValue::Text(text) => Ok(text.clone()),
                HeaderValue::Bool(_) => Err(format!("`{key}` should be text in quotes")),
            };

            let result: Result<(), String> = match key.as_str() {
                "name" => text(&value).map(|text| header.name = Some(text)),
                "description" => text(&value).map(|text| header.description = Some(text)),
                "mode" => text(&value).and_then(|text| match text.as_str() {
                    "background" => {
                        header.mode = Mode::Background;
                        Ok(())
                    }
                    "terminal" => {
                        header.mode = Mode::Terminal;
                        Ok(())
                    }
                    other => Err(format!("mode `{other}` is not one of background, terminal")),
                }),
                "bind" => text(&value).and_then(|text| {
                    Binding::parse(&text)
                        .map(|_| header.bind = Some(text))
                        .map_err(|err| err.to_string())
                }),
                "autostart" => match value {
                    HeaderValue::Bool(on) => {
                        header.autostart = on;
                        Ok(())
                    }
                    HeaderValue::Text(_) => Err("autostart should be true or false".into()),
                },
                other => Err(format!(
                    "`{other}` is not a header key (name, description, mode, bind, autostart)"
                )),
            };
            if let Err(reason) = result {
                header.problems.push(problem(reason));
            }
        }
        header
    }
}

/// A header value: TOML-style text or a boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderValue {
    Text(String),
    Bool(bool),
}

/// The part of a header line after `# tessera:`, if this is a header line.
fn header_body(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    rest.strip_prefix(MARKER).map(str::trim)
}

/// Parses `key = value`, where value is TOML (quoted text, `true`, `false`),
/// optionally followed by a `#` comment. A bare word is accepted as text too,
/// so `mode = terminal` works as people will inevitably write it.
fn parse_assignment(body: &str) -> Result<(String, HeaderValue), String> {
    let Some((key, _)) = body.split_once('=') else {
        return Err(format!("`{body}` is not `key = value`"));
    };
    let key = key.trim();
    if key.is_empty()
        || !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(format!("`{key}` is not a key name"));
    }

    // TOML already knows about quotes, escapes and trailing comments.
    if let Ok(document) = body.parse::<toml_edit::DocumentMut>()
        && let Some(item) = document.get(key)
    {
        if let Some(text) = item.as_str() {
            return Ok((key.to_string(), HeaderValue::Text(text.to_string())));
        }
        if let Some(on) = item.as_bool() {
            return Ok((key.to_string(), HeaderValue::Bool(on)));
        }
        return Err(format!("`{key}` should be text in quotes, true or false"));
    }

    let raw = body.split_once('=').map(|(_, value)| value).unwrap_or("");
    let bare = raw.split('#').next().unwrap_or("").trim();
    if !bare.is_empty() && !bare.contains(char::is_whitespace) && !bare.contains('"') {
        return Ok((key.to_string(), HeaderValue::Text(bare.to_string())));
    }
    Err(format!(
        "could not read the value of `{key}`; put text in double quotes"
    ))
}

/// Returns the script text with its `autostart` header set to `on`.
///
/// An existing `autostart` line is rewritten in place, keeping its
/// indentation and any trailing comment. Otherwise a line is added after the
/// last header line, or after the `#!` line, or at the very top.
pub fn set_autostart(text: &str, on: bool) -> String {
    let value = if on { "true" } else { "false" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();

    let mut last_header = None;
    for (index, line) in lines.iter_mut().enumerate().take(HEADER_LINES) {
        let Some(body) = header_body(line) else {
            continue;
        };
        last_header = Some(index);
        let is_autostart = body
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "autostart");
        if !is_autostart {
            continue;
        }
        // Keep everything up to and including "=", and any "# comment" after the value.
        let (before, after) = line.split_once('=').expect("checked above");
        let ending = if after.ends_with('\n') { "\n" } else { "" };
        let comment = after
            .trim_end_matches('\n')
            .split_once('#')
            .map(|(_, comment)| format!("  #{comment}"))
            .unwrap_or_default();
        *line = format!("{before}= {value}{comment}{ending}");
        return lines.concat();
    }

    let new_line = format!("# {MARKER} autostart = {value}\n");
    let position = match last_header {
        Some(index) => index + 1,
        None if lines.first().is_some_and(|line| line.starts_with("#!")) => 1,
        None => 0,
    };
    // Adding after a final line with no newline would glue the two together.
    if let Some(previous) = position.checked_sub(1).and_then(|i| lines.get_mut(i))
        && !previous.ends_with('\n')
    {
        previous.push('\n');
    }
    lines.insert(position, new_line);
    lines.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "#!/bin/sh
# tessera: name = \"Sync notes\"
# tessera: description = \"Push ~/notes to the remote and pull changes\"
# tessera: mode = \"background\"        # background | terminal
# tessera: bind = \"Mod+Shift+N\"
# tessera: autostart = false
set -eu
cd ~/notes && git pull --rebase && git push
";

    #[test]
    fn the_design_example_parses() {
        let header = Header::parse(EXAMPLE);
        assert_eq!(header.name.as_deref(), Some("Sync notes"));
        assert_eq!(
            header.description.as_deref(),
            Some("Push ~/notes to the remote and pull changes")
        );
        assert_eq!(header.mode, Mode::Background);
        assert_eq!(header.bind.as_deref(), Some("Mod+Shift+N"));
        assert!(!header.autostart);
        assert!(header.problems.is_empty(), "{:?}", header.problems);
    }

    #[test]
    fn a_script_without_a_header_gets_defaults() {
        let header = Header::parse("#!/bin/sh\necho hi\n");
        assert_eq!(header, Header::default());
    }

    #[test]
    fn bare_words_and_spacing_variations_are_accepted() {
        let header = Header::parse("#tessera:mode=terminal\n  #  tessera: autostart = true\n");
        assert_eq!(header.mode, Mode::Terminal);
        assert!(header.autostart);
        assert!(header.problems.is_empty(), "{:?}", header.problems);
    }

    #[test]
    fn malformed_lines_are_reported_and_skipped() {
        let header = Header::parse(
            "#!/bin/sh
# tessera: name \"no equals sign\"
# tessera: mode = \"sideways\"
# tessera: colour = \"blue\"
# tessera: autostart = \"yes\"
# tessera: bind = \"Shift+N\"
# tessera: name = \"Still read\"
",
        );
        assert_eq!(
            header.name.as_deref(),
            Some("Still read"),
            "good lines still apply"
        );
        assert_eq!(header.problems.len(), 5, "{:#?}", header.problems);
        assert!(header.problems[0].starts_with("line 2:"));
        assert!(header.problems[1].contains("sideways"));
        assert!(header.problems[2].contains("colour"));
        assert!(header.problems[3].contains("true or false"));
        assert!(header.problems[4].contains("Mod"), "{}", header.problems[4]);
        assert_eq!(header.bind, None);
    }

    #[test]
    fn only_the_first_twenty_lines_count() {
        let mut text = "#!/bin/sh\n".repeat(HEADER_LINES);
        text.push_str("# tessera: name = \"too late\"\n");
        assert_eq!(Header::parse(&text).name, None);
    }

    #[test]
    fn ordinary_comments_are_not_header_lines() {
        let header = Header::parse("# this script is for tessera: it syncs\n# name = x\n");
        assert_eq!(header, Header::default());
    }

    #[test]
    fn autostart_is_rewritten_in_place() {
        let text = set_autostart(EXAMPLE, true);
        assert!(text.contains("# tessera: autostart = true\n"), "{text}");
        assert!(!text.contains("autostart = false"));
        assert_eq!(text.lines().count(), EXAMPLE.lines().count());
        assert!(Header::parse(&text).autostart);
    }

    #[test]
    fn a_trailing_comment_on_the_autostart_line_survives() {
        let text = set_autostart("# tessera: autostart = false  # at login\n", true);
        assert_eq!(text, "# tessera: autostart = true  # at login\n");
    }

    #[test]
    fn autostart_is_added_after_the_header_or_the_shebang() {
        let text = set_autostart("#!/bin/sh\n# tessera: name = \"x\"\necho\n", true);
        assert_eq!(
            text,
            "#!/bin/sh\n# tessera: name = \"x\"\n# tessera: autostart = true\necho\n"
        );

        let text = set_autostart("#!/bin/sh\necho\n", true);
        assert_eq!(text, "#!/bin/sh\n# tessera: autostart = true\necho\n");

        let text = set_autostart("#!/bin/sh", true);
        assert_eq!(text, "#!/bin/sh\n# tessera: autostart = true\n");

        let text = set_autostart("echo\n", false);
        assert_eq!(text, "# tessera: autostart = false\necho\n");
    }
}
