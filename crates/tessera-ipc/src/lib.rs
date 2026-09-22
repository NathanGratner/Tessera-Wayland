//! Messages between the Tessera compositor and its launcher (design §4).
//!
//! Wayland deliberately gives clients no say over where their windows go, so
//! requests like "open this program next to me" travel over a private Unix
//! socket instead. One JSON object per line, so the protocol can be driven by
//! hand with `socat` and read in a log.
//!
//! ```no_run
//! use tessera_ipc::{Placement, Request, Side};
//!
//! let request = Request::Spawn {
//!     argv: vec!["foot".into()],
//!     placement: Placement::BesideCaller { side: Side::Right, ratio: 0.6 },
//! };
//! ```

#![warn(missing_docs)]

use std::{
    ffi::OsString,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

/// Environment variable naming the socket, set for every process the compositor starts.
pub const SOCKET_ENV: &str = "TESSERA_SOCKET";

/// A window, as far as the launcher is concerned. Ids are not reused within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WindowId(pub u64);

/// Which side of the anchor window a new window goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// To the left of the anchor.
    Left,
    /// To the right of the anchor. The default, as in "a terminal to the
    /// right of the launcher".
    #[default]
    Right,
    /// Above the anchor.
    Above,
    /// Below the anchor.
    Below,
}

impl Side {
    /// The other side: Left and Right, Above and Below.
    pub fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Above => Side::Below,
            Side::Below => Side::Above,
        }
    }

    /// Parses `left`, `right`, `above` or `below`, as written in messages and config.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "left" => Side::Left,
            "right" => Side::Right,
            "above" => Side::Above,
            "below" => Side::Below,
            _ => return None,
        })
    }
}

/// Where a newly spawned window should go.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Placement {
    /// Tile it the usual way: split the focused window along its longer side.
    Auto,
    /// Split the *caller's own* window, putting the new one on `side` with
    /// `ratio` of that space. The compositor identifies the caller from the
    /// socket's peer credentials, so the launcher never names its own window.
    BesideCaller {
        /// Which side of the caller's window to use.
        side: Side,
        /// The new window's share of the space, 0.1–0.9.
        ratio: f32,
    },
    /// Put it on this workspace (1–9) instead of the active one.
    Workspace {
        /// Workspace number as shown to the user, 1–9.
        number: u8,
    },
}

/// A request from a client of the compositor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Request {
    /// Run a program as a client of the compositor and place its window.
    Spawn {
        /// Program and arguments. Not passed through a shell.
        argv: Vec<String>,
        /// Where the window should go once it appears.
        #[serde(default = "Placement::auto")]
        placement: Placement,
    },
    /// List the windows the compositor knows about.
    ListWindows,
    /// Give keyboard focus to a window, switching workspace if needed.
    Focus {
        /// The window to focus.
        window: WindowId,
    },
    /// Re-read the configuration file and apply what can be applied now.
    ReloadConfig,
    /// List the scripts in the scripts folder, with their state.
    ListScripts,
    /// Run a script. Refused if it is already running in the background.
    RunScript {
        /// The script's file name, as in [`ScriptInfo::name`].
        name: String,
        /// How to run it; the script's own header decides when absent.
        #[serde(default)]
        mode: Option<RunMode>,
    },
    /// Stop a running script: SIGTERM, then SIGKILL after 3 seconds.
    StopScript {
        /// The script's file name.
        name: String,
    },
    /// The last lines a script printed, from its most recent background run.
    ScriptOutput {
        /// The script's file name.
        name: String,
        /// How many lines, counting back from the newest.
        #[serde(default = "default_tail")]
        tail: usize,
    },
    /// Keep this connection open and send every [`Event`] down it, one per line,
    /// after an initial [`Response::Ok`].
    Subscribe,
    /// End the session: the compositor answers [`Response::Ok`], then exits.
    Quit,
}

fn default_tail() -> usize {
    500
}

/// How a script runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// In the background, with output captured by the compositor.
    #[default]
    Background,
    /// In a terminal tiled beside the caller, showing its own output.
    Terminal,
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitStatus {
    /// It exited by itself with this code.
    Code {
        /// The exit code; 0 is success.
        code: i32,
    },
    /// It was killed by this signal.
    Signal {
        /// The signal number, e.g. 15 for SIGTERM.
        signal: i32,
    },
}

impl ExitStatus {
    /// Whether it exited with code 0.
    pub fn success(self) -> bool {
        self == ExitStatus::Code { code: 0 }
    }

    /// A short description: `ok`, `exit 1`, `SIGTERM`.
    pub fn describe(self) -> String {
        match self {
            ExitStatus::Code { code: 0 } => "ok".into(),
            ExitStatus::Code { code } => format!("exit {code}"),
            ExitStatus::Signal { signal } => match signal_name(signal) {
                Some(name) => name.into(),
                None => format!("signal {signal}"),
            },
        }
    }
}

/// The usual name of a Linux signal number.
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return None,
    })
}

/// Where a script is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScriptState {
    /// Not run since the compositor started.
    NeverRun,
    /// Running in the background.
    Running {
        /// Its process id.
        pid: u32,
        /// When it started, in milliseconds since the Unix epoch.
        started_at_ms: u64,
    },
    /// Opened in a terminal, whose output the compositor does not see.
    InTerminal {
        /// When it was opened, in milliseconds since the Unix epoch.
        started_at_ms: u64,
    },
    /// Finished.
    Exited {
        /// How it ended.
        exit: ExitStatus,
        /// When it started, in milliseconds since the Unix epoch.
        started_at_ms: u64,
        /// How long it ran.
        duration_ms: u64,
    },
}

/// One script in a [`Response::Scripts`] listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptInfo {
    /// The file name, which identifies the script in requests.
    pub name: String,
    /// What to call it: the header's `name`, or the file name.
    pub title: String,
    /// The header's `description`, if any.
    pub description: Option<String>,
    /// Full path to the file.
    pub path: String,
    /// How it runs by default.
    pub mode: RunMode,
    /// The key binding in effect, if any. A binding that clashed is absent
    /// here and explained in `problems`.
    pub bind: Option<String>,
    /// Runs when the compositor starts.
    pub autostart: bool,
    /// Header lines that could not be read, and binding conflicts.
    #[serde(default)]
    pub problems: Vec<String>,
    /// Running, finished, or never run.
    pub state: ScriptState,
}

/// One line of captured output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLine {
    /// The text, without its newline.
    pub text: String,
    /// True when it came from stderr.
    #[serde(default)]
    pub stderr: bool,
}

/// Milliseconds since the Unix epoch, the time format used in messages.
pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

impl Placement {
    fn auto() -> Self {
        Placement::Auto
    }
}

/// One window in a [`Response::Windows`] listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Stable id for use with [`Request::Focus`].
    pub id: WindowId,
    /// The window's `xdg_toplevel` app id, if it set one.
    pub app_id: Option<String>,
    /// The window's title, if it set one.
    pub title: Option<String>,
    /// Workspace number, 1–9.
    pub workspace: u8,
    /// Whether this window currently has keyboard focus.
    pub focused: bool,
}

/// The compositor's answer to a [`Request`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// The request succeeded and has no result.
    Ok,
    /// A program was started; its window will appear shortly.
    Spawned {
        /// Process id of the program that was started.
        pid: u32,
    },
    /// The configuration was reloaded.
    ConfigApplied {
        /// Settings that changed and are already in effect.
        live: Vec<String>,
        /// Settings that changed but only take effect after a restart.
        needs_restart: Vec<String>,
    },
    /// The windows the compositor knows about.
    Windows {
        /// One entry per window, in tiling order.
        windows: Vec<WindowInfo>,
    },
    /// The scripts in the scripts folder, sorted by title.
    Scripts {
        /// One entry per script.
        scripts: Vec<ScriptInfo>,
    },
    /// A script's captured output.
    ScriptOutput {
        /// The script's file name.
        name: String,
        /// Oldest first.
        lines: Vec<OutputLine>,
        /// Whether it is still running, so more may follow.
        running: bool,
    },
    /// The request could not be carried out.
    Error {
        /// Why, in terms a person can act on.
        message: String,
    },
}

/// Something that happened, sent to clients that sent [`Request::Subscribe`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    /// A window was mapped.
    WindowOpened {
        /// The new window.
        id: WindowId,
        /// Its app id, if it set one.
        app_id: Option<String>,
    },
    /// A window went away.
    WindowClosed {
        /// The window that closed.
        id: WindowId,
    },
    /// A script started, in the background or in a terminal.
    ScriptStarted {
        /// The script's file name.
        name: String,
        /// Process id of the script, or of its terminal.
        pid: u32,
    },
    /// A background script finished.
    ScriptExited {
        /// The script's file name.
        name: String,
        /// How it ended.
        exit: ExitStatus,
        /// How long it ran.
        duration_ms: u64,
    },
    /// Scripts were added, removed or edited; ask for the list again.
    ScriptsChanged,
}

/// The socket path for a given Wayland display name.
///
/// `$XDG_RUNTIME_DIR/tessera-<display>.sock`, falling back to `/tmp` when the
/// runtime directory is not set.
pub fn socket_path(wayland_display: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| OsString::from("/tmp"));
    PathBuf::from(dir).join(format!("tessera-{wayland_display}.sock"))
}

/// The socket this process should talk to, from [`SOCKET_ENV`].
///
/// Returns `None` when the process was not started by Tessera.
pub fn socket_from_env() -> Option<PathBuf> {
    std::env::var_os(SOCKET_ENV).map(PathBuf::from)
}

/// Writes one message as a single line of JSON.
pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one message, or `None` at end of input.
pub fn read_message<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    if line.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let mut buffer = Vec::new();
        write_message(&mut buffer, value).unwrap();
        assert!(buffer.ends_with(b"\n"), "messages are newline delimited");
        assert_eq!(buffer.iter().filter(|byte| **byte == b'\n').count(), 1);
        read_message(&mut buffer.as_slice()).unwrap().unwrap()
    }

    #[test]
    fn requests_survive_a_round_trip() {
        for request in [
            Request::Spawn {
                argv: vec!["foot".into(), "-e".into(), "htop".into()],
                placement: Placement::Auto,
            },
            Request::Spawn {
                argv: vec!["foot".into()],
                placement: Placement::BesideCaller {
                    side: Side::Right,
                    ratio: 0.6,
                },
            },
            Request::Spawn {
                argv: vec!["firefox".into()],
                placement: Placement::Workspace { number: 3 },
            },
            Request::ListWindows,
            Request::Focus {
                window: WindowId(7),
            },
            Request::ReloadConfig,
            Request::ListScripts,
            Request::RunScript {
                name: "sync-notes".into(),
                mode: Some(RunMode::Terminal),
            },
            Request::StopScript {
                name: "sync-notes".into(),
            },
            Request::ScriptOutput {
                name: "sync-notes".into(),
                tail: 50,
            },
            Request::Subscribe,
            Request::Quit,
        ] {
            assert_eq!(round_trip(&request), request);
        }
    }

    #[test]
    fn responses_survive_a_round_trip() {
        for response in [
            Response::Ok,
            Response::Spawned { pid: 1234 },
            Response::ConfigApplied {
                live: vec!["layout.gaps".into()],
                needs_restart: vec!["launcher.font".into()],
            },
            Response::Windows {
                windows: vec![WindowInfo {
                    id: WindowId(1),
                    app_id: Some("tessera.launcher".into()),
                    title: None,
                    workspace: 1,
                    focused: true,
                }],
            },
            Response::Error {
                message: "no such window".into(),
            },
            Response::Scripts {
                scripts: vec![ScriptInfo {
                    name: "backup".into(),
                    title: "Back up home".into(),
                    description: None,
                    path: "/home/me/.config/tessera/scripts/backup".into(),
                    mode: RunMode::Background,
                    bind: Some("Mod+Shift+B".into()),
                    autostart: false,
                    problems: vec!["line 3: `colour` is not a header key".into()],
                    state: ScriptState::Exited {
                        exit: ExitStatus::Signal { signal: 15 },
                        started_at_ms: 1_000,
                        duration_ms: 42,
                    },
                }],
            },
            Response::ScriptOutput {
                name: "backup".into(),
                lines: vec![OutputLine {
                    text: "rsync: done".into(),
                    stderr: false,
                }],
                running: true,
            },
        ] {
            assert_eq!(round_trip(&response), response);
        }
    }

    #[test]
    fn events_survive_a_round_trip() {
        for event in [
            Event::WindowOpened {
                id: WindowId(2),
                app_id: None,
            },
            Event::WindowClosed { id: WindowId(2) },
            Event::ScriptStarted {
                name: "backup".into(),
                pid: 99,
            },
            Event::ScriptExited {
                name: "backup".into(),
                exit: ExitStatus::Code { code: 1 },
                duration_ms: 1500,
            },
            Event::ScriptsChanged,
        ] {
            assert_eq!(round_trip(&event), event);
        }
    }

    #[test]
    fn requests_are_readable_and_writable_by_hand() {
        // The shape `socat` users and log readers see.
        let request: Request = serde_json::from_str(r#"{"type":"ListWindows"}"#).unwrap();
        assert_eq!(request, Request::ListWindows);

        let request: Request = serde_json::from_str(r#"{"type":"Spawn","argv":["foot"]}"#).unwrap();
        assert_eq!(
            request,
            Request::Spawn {
                argv: vec!["foot".into()],
                placement: Placement::Auto,
            },
            "placement defaults to Auto when omitted"
        );

        let json = serde_json::to_string(&Request::Spawn {
            argv: vec!["foot".into()],
            placement: Placement::BesideCaller {
                side: Side::Right,
                ratio: 0.6,
            },
        })
        .unwrap();
        assert!(json.contains(r#""type":"beside_caller""#), "{json}");
        assert!(json.contains(r#""side":"right""#), "{json}");
    }

    #[test]
    fn script_requests_are_short_by_hand() {
        let request: Request =
            serde_json::from_str(r#"{"type":"RunScript","name":"backup"}"#).unwrap();
        assert_eq!(
            request,
            Request::RunScript {
                name: "backup".into(),
                mode: None
            }
        );
        let request: Request =
            serde_json::from_str(r#"{"type":"ScriptOutput","name":"backup"}"#).unwrap();
        assert_eq!(
            request,
            Request::ScriptOutput {
                name: "backup".into(),
                tail: 500
            }
        );
    }

    #[test]
    fn sides_have_opposites_and_names() {
        for side in [Side::Left, Side::Right, Side::Above, Side::Below] {
            assert_eq!(side.opposite().opposite(), side);
            let name = serde_json::to_string(&side).unwrap();
            assert_eq!(Side::from_name(name.trim_matches('"')), Some(side));
        }
        assert_eq!(Side::from_name("up"), None);
    }

    #[test]
    fn exit_statuses_describe_themselves() {
        assert_eq!(ExitStatus::Code { code: 0 }.describe(), "ok");
        assert!(ExitStatus::Code { code: 0 }.success());
        assert_eq!(ExitStatus::Code { code: 2 }.describe(), "exit 2");
        assert_eq!(ExitStatus::Signal { signal: 15 }.describe(), "SIGTERM");
        assert_eq!(ExitStatus::Signal { signal: 64 }.describe(), "signal 64");
    }

    #[test]
    fn reading_stops_at_end_of_input() {
        let mut empty = &b""[..];
        assert_eq!(read_message::<Request>(&mut empty).unwrap(), None);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        let mut bad = &b"{not json}\n"[..];
        assert!(read_message::<Request>(&mut bad).is_err());
    }

    #[test]
    fn socket_path_follows_the_runtime_directory() {
        let path = socket_path("wayland-1");
        assert!(
            path.to_string_lossy().ends_with("tessera-wayland-1.sock"),
            "{path:?}"
        );
    }
}
