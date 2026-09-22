//! The private socket the launcher talks to (design §4).
//!
//! Wayland gives clients no control over window placement, so requests like
//! "open this program next to me" arrive here instead. The caller is identified
//! by the socket's peer credentials, which is what lets the launcher say
//! *beside me* without naming its own window.

use std::{
    io::{ErrorKind, Read},
    os::unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, anyhow};
use smithay::{
    desktop::Window,
    reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic},
};
use tessera_ipc::{Event, Request, Response, WindowInfo};

use crate::{layout::Direction, state::Tessera};

/// Whether an IPC connection is still worth watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Connection {
    /// Still open; keep listening for more requests.
    Open,
    /// The client hung up or failed.
    Closed,
}

/// How long a spawned program has to show its window before we stop waiting.
pub const PLACEMENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Where a window that has not appeared yet should go.
#[derive(Debug, Clone)]
pub enum PendingKind {
    /// Tile it the usual way.
    Auto,
    /// Split this window, putting the new one on `side` with `ratio` of the space.
    Beside {
        /// The window to split.
        anchor: Window,
        /// Which side the new window takes.
        side: Direction,
        /// The new window's share, 0.1–0.9.
        ratio: f32,
    },
    /// Put it on this workspace index (0-based).
    Workspace(usize),
}

/// A spawn we are still waiting on.
#[derive(Debug, Clone)]
pub struct PendingPlacement {
    /// Process id we started.
    pub pid: u32,
    /// The xdg-activation token handed to it, if any.
    pub token: Option<String>,
    /// Where its window should go.
    pub kind: PendingKind,
    /// When to give up waiting.
    pub deadline: Instant,
}

/// The parts of a [`PendingPlacement`] that matching looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMeta {
    /// Process id we started.
    pub pid: u32,
    /// Activation token handed to that process.
    pub token: Option<String>,
    /// When it expires.
    pub deadline: Instant,
}

/// Decides which pending spawn a new window belongs to (design §8).
///
/// Tried in order: the activation token the client presented, then the process
/// id (or one of its descendants), then the oldest spawn still inside its
/// deadline. Expired entries never match.
pub fn match_pending(
    pending: &[PendingMeta],
    pid: Option<u32>,
    token: Option<&str>,
    now: Instant,
    is_descendant: impl Fn(u32, u32) -> bool,
) -> Option<usize> {
    let live = |entry: &PendingMeta| entry.deadline > now;

    if let Some(token) = token
        && let Some(index) = pending
            .iter()
            .position(|entry| live(entry) && entry.token.as_deref() == Some(token))
    {
        return Some(index);
    }

    if let Some(pid) = pid
        && let Some(index) = pending
            .iter()
            .position(|entry| live(entry) && (entry.pid == pid || is_descendant(pid, entry.pid)))
    {
        return Some(index);
    }

    // Last resort: a client that neither honours activation tokens nor runs in
    // the process we started, e.g. a terminal served by an existing daemon.
    pending
        .iter()
        .enumerate()
        .filter(|(_, entry)| live(entry))
        .min_by_key(|(_, entry)| entry.deadline)
        .map(|(index, _)| index)
}

/// Whether `pid` is `ancestor` or one of its descendants, walking `/proc`.
pub fn is_descendant(pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    // A shell wrapper plus a re-exec is normally two hops; ten is plenty.
    for _ in 0..10 {
        if current == ancestor {
            return true;
        }
        match parent_of(current) {
            Some(parent) if parent > 1 => current = parent,
            _ => return false,
        }
    }
    false
}

/// Reads the parent process id from `/proc/<pid>/stat`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 4 is the ppid, but field 2 is the command and may contain spaces
    // and brackets, so start after the closing bracket.
    let after_command = stat.rsplit_once(')')?.1;
    after_command.split_whitespace().nth(1)?.parse().ok()
}

/// The listening socket. Removing it on drop keeps stale sockets from piling up.
#[derive(Debug)]
pub struct IpcServer {
    path: PathBuf,
}

impl IpcServer {
    /// Binds the socket and starts accepting connections on the event loop.
    pub fn bind(handle: &LoopHandle<'static, Tessera>, path: &Path) -> anyhow::Result<Self> {
        // A socket left behind by a crashed compositor would block binding.
        if path.exists() {
            std::fs::remove_file(path).ok();
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("failed to bind the IPC socket at {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .context("failed to set the IPC socket non-blocking")?;
        // Only this user may talk to the compositor.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .context("failed to restrict the IPC socket")?;

        handle
            .insert_source(
                Generic::new(listener, Interest::READ, Mode::Level),
                |_, listener, state| {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => state.add_ipc_client(stream),
                            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                            Err(err) => {
                                tracing::warn!(%err, "failed to accept an IPC connection");
                                break;
                            }
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|err| anyhow!("failed to watch the IPC socket: {}", err.error))?;

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Where the socket lives; handed to children as `TESSERA_SOCKET`.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

impl Tessera {
    /// Takes over an accepted connection.
    ///
    /// A client that sends one request and closes (`echo | socat`, and the
    /// launcher) may have its data *and* its EOF waiting before the connection
    /// is registered with the event loop, and that first readiness is never
    /// delivered. So the connection is drained once here, and only registered
    /// if it is still open.
    pub(crate) fn add_ipc_client(&mut self, stream: UnixStream) {
        // `UnixStream::peer_cred` is still unstable, so ask for SO_PEERCRED directly.
        let pid = rustix::net::sockopt::socket_peercred(&stream)
            .ok()
            .map(|credentials| credentials.pid.as_raw_nonzero().get() as u32);
        if let Err(err) = stream.set_nonblocking(true) {
            tracing::warn!(%err, "failed to set an IPC connection non-blocking");
            return;
        }
        let id = self.next_connection_id;
        self.next_connection_id += 1;
        tracing::debug!(?pid, id, "ipc client connected");

        let mut buffer = Vec::new();
        if self.pump_ipc(&stream, &mut buffer, pid, id) == Connection::Closed {
            return;
        }

        let source = Generic::new(stream, Interest::READ, Mode::Level);
        let inserted = self
            .loop_handle
            .insert_source(source, move |_, stream, state| {
                match state.pump_ipc(stream, &mut buffer, pid, id) {
                    Connection::Open => Ok(PostAction::Continue),
                    Connection::Closed => Ok(PostAction::Remove),
                }
            });

        if let Err(err) = inserted {
            tracing::warn!(error = %err.error, "failed to watch an IPC connection");
        }
    }

    /// Reads whatever is waiting, answers any complete requests, and reports
    /// whether the connection is still open.
    fn pump_ipc(
        &mut self,
        stream: &UnixStream,
        buffer: &mut Vec<u8>,
        pid: Option<u32>,
        id: u64,
    ) -> Connection {
        // Read and Write are implemented for `&UnixStream`, so the calloop
        // wrapper's immutable deref is enough and no unsafe is needed.
        let mut connection: &UnixStream = stream;
        let mut chunk = [0u8; 4096];
        let mut closed = false;

        loop {
            match connection.read(&mut chunk) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    tracing::warn!(%err, "ipc read failed");
                    self.forget_subscriber(id);
                    return Connection::Closed;
                }
            }
        }

        while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=position).collect();
            let (response, subscribe) = self.handle_ipc_line(&line, pid);
            if let Err(err) = tessera_ipc::write_message(&mut connection, &response) {
                tracing::warn!(%err, "ipc write failed");
                self.forget_subscriber(id);
                return Connection::Closed;
            }
            if subscribe && !closed {
                match stream.try_clone() {
                    Ok(writer) => self.subscribers.push((id, writer)),
                    Err(err) => tracing::warn!(%err, "cannot keep a subscriber"),
                }
            }
        }

        if closed {
            self.forget_subscriber(id);
            Connection::Closed
        } else {
            Connection::Open
        }
    }

    fn forget_subscriber(&mut self, id: u64) {
        self.subscribers.retain(|(known, _)| *known != id);
    }

    /// Sends an event to every subscriber, dropping any that have gone away.
    ///
    /// Events are small and the socket buffer large, so a subscriber too slow
    /// to take one line is treated as gone rather than queued for.
    pub(crate) fn broadcast(&mut self, event: &Event) {
        self.subscribers.retain(|(id, stream)| {
            let mut writer: &UnixStream = stream;
            match tessera_ipc::write_message(&mut writer, event) {
                Ok(()) => true,
                Err(err) => {
                    tracing::debug!(%err, id, "dropping an event subscriber");
                    false
                }
            }
        });
    }

    /// Parses one request line and carries it out. The flag is true for
    /// `Subscribe`, whose connection must be kept for events.
    fn handle_ipc_line(&mut self, line: &[u8], peer_pid: Option<u32>) -> (Response, bool) {
        let request: Request = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(err) => {
                let message = format!("could not understand the request: {err}");
                return (Response::Error { message }, false);
            }
        };
        tracing::debug!(?request, ?peer_pid, "ipc request");
        let subscribe = request == Request::Subscribe;
        (self.handle_ipc_request(request, peer_pid), subscribe)
    }

    /// Carries out one request. Split out from parsing so it can be called directly.
    pub(crate) fn handle_ipc_request(
        &mut self,
        request: Request,
        peer_pid: Option<u32>,
    ) -> Response {
        match request {
            Request::ListWindows => Response::Windows {
                windows: self.window_list(),
            },
            Request::Focus { window } => match self.window_by_id(window) {
                Some(window) => {
                    self.focus_anywhere(&window);
                    Response::Ok
                }
                None => Response::Error {
                    message: format!("no window with id {}", window.0),
                },
            },
            Request::ReloadConfig => self.reload_config(),
            Request::Subscribe => Response::Ok,
            Request::Quit => {
                // The answer is written before the loop gets round to stopping.
                tracing::info!(?peer_pid, "quit requested over IPC");
                self.loop_signal.stop();
                Response::Ok
            }
            Request::ListScripts => Response::Scripts {
                scripts: self.script_infos(),
            },
            Request::RunScript { name, mode } => match self.script_index(&name) {
                Some(index) => match self.run_script(index, mode, peer_pid) {
                    Ok(pid) => Response::Spawned { pid },
                    Err(message) => Response::Error { message },
                },
                None => no_such_script(&name),
            },
            Request::StopScript { name } => match self.script_index(&name) {
                Some(index) => match self.stop_script(index) {
                    Ok(()) => Response::Ok,
                    Err(message) => Response::Error { message },
                },
                None => no_such_script(&name),
            },
            Request::ScriptOutput { name, tail } => match self.script_index(&name) {
                Some(index) => {
                    let (lines, running) = self.script_output(index, tail);
                    Response::ScriptOutput {
                        name,
                        lines,
                        running,
                    }
                }
                None => no_such_script(&name),
            },
            Request::Spawn { argv, placement } => {
                if argv.is_empty() {
                    return Response::Error {
                        message: "spawn needs a command".into(),
                    };
                }
                match self.spawn_placed(&argv, placement, peer_pid) {
                    Ok(pid) => Response::Spawned { pid },
                    Err(err) => Response::Error {
                        message: format!("{err:#}"),
                    },
                }
            }
        }
    }

    /// Every window the compositor knows about, in tiling order.
    fn window_list(&self) -> Vec<WindowInfo> {
        let mut windows = Vec::new();
        for (index, workspace) in self.workspaces.iter().enumerate() {
            for window in workspace.ids() {
                let Some(id) = self.id_of_window(&window) else {
                    continue;
                };
                windows.push(WindowInfo {
                    id,
                    app_id: crate::layout::apply::window_app_id(&window),
                    title: crate::layout::apply::window_title(&window),
                    workspace: index as u8 + 1,
                    focused: self.focus.as_ref() == Some(&window),
                });
            }
        }
        windows
    }
}

fn no_such_script(name: &str) -> Response {
    Response::Error {
        message: format!("no script called `{name}` in the scripts folder"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(pid: u32, token: Option<&str>, deadline: Instant) -> PendingMeta {
        PendingMeta {
            pid,
            token: token.map(str::to_string),
            deadline,
        }
    }

    fn never(_: u32, _: u32) -> bool {
        false
    }

    #[test]
    fn an_activation_token_wins_over_everything() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(1);
        let pending = vec![meta(10, Some("tok-a"), soon), meta(20, Some("tok-b"), soon)];
        assert_eq!(
            match_pending(&pending, Some(10), Some("tok-b"), now, never),
            Some(1)
        );
    }

    #[test]
    fn the_process_id_is_the_next_best_thing() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(1);
        let pending = vec![meta(10, Some("tok-a"), soon), meta(20, None, soon)];
        assert_eq!(match_pending(&pending, Some(20), None, now, never), Some(1));
        // An unknown token falls through to the pid.
        assert_eq!(
            match_pending(&pending, Some(20), Some("other"), now, never),
            Some(1)
        );
    }

    #[test]
    fn a_descendant_process_counts_as_the_one_we_started() {
        let now = Instant::now();
        let soon = now + Duration::from_secs(1);
        let pending = vec![meta(10, None, soon)];
        let descends = |child: u32, ancestor: u32| child == 99 && ancestor == 10;
        assert_eq!(
            match_pending(&pending, Some(99), None, now, descends),
            Some(0)
        );
    }

    #[test]
    fn an_unrecognised_window_takes_the_oldest_waiting_spawn() {
        // The single-instance terminal case: the window comes from a daemon we
        // never started, with no token.
        let now = Instant::now();
        let pending = vec![
            meta(10, Some("tok"), now + Duration::from_secs(2)),
            meta(20, None, now + Duration::from_secs(1)),
        ];
        assert_eq!(
            match_pending(&pending, Some(999), None, now, never),
            Some(1)
        );
    }

    #[test]
    fn expired_spawns_never_match() {
        let now = Instant::now();
        let past = now - Duration::from_secs(1);
        let pending = vec![meta(10, Some("tok"), past)];
        assert_eq!(
            match_pending(&pending, Some(10), Some("tok"), now, never),
            None
        );
        assert_eq!(match_pending(&[], Some(1), None, now, never), None);
    }

    #[test]
    fn a_process_is_its_own_descendant_but_not_of_a_stranger() {
        let me = std::process::id();
        assert!(is_descendant(me, me));
        assert!(!is_descendant(me, u32::MAX - 1));
    }

    #[test]
    fn the_parent_of_this_process_is_readable() {
        // Exercises the /proc/<pid>/stat parsing, including command names with spaces.
        let parent = parent_of(std::process::id());
        assert!(parent.is_some(), "should be able to read our own ppid");
        assert!(is_descendant(std::process::id(), parent.unwrap()));
    }
}
