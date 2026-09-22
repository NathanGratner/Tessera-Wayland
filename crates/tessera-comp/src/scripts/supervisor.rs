//! Running background scripts (design §7).
//!
//! Each script runs in its own process group with stdout and stderr on pipes.
//! Three kinds of event source tell the event loop what it is doing, so there
//! is no SIGCHLD handling and no thread per script:
//!
//! - each pipe, when there is output to read or it closes;
//! - a pidfd, which becomes readable when the process exits;
//! - a timer, three seconds after asking it to stop, to kill it if it did not.

use std::{
    fs::File,
    io::{ErrorKind, Read},
    os::{
        fd::OwnedFd,
        unix::process::{CommandExt, ExitStatusExt},
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use rustix::process::{Pid, PidfdFlags, Signal, kill_process_group, pidfd_open};
use smithay::reexports::calloop::{
    Interest, Mode, PostAction,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};
use tessera_ipc::{Event, ExitStatus, ScriptState};

use super::millis_since;
use crate::state::Tessera;

/// How long a script gets to exit after SIGTERM before SIGKILL (design §7).
const STOP_GRACE: Duration = Duration::from_secs(3);

/// A background script that is still running.
#[derive(Debug)]
pub struct Run {
    /// Distinguishes this run from earlier and later ones of the same script.
    id: u64,
    child: Child,
    pid: u32,
    started: Instant,
    started_at_ms: u64,
    /// Second handles on the pipes, to read what is left when the process exits.
    pipes: Vec<(bool, File)>,
}

impl Tessera {
    /// Starts a script in the background, capturing its output.
    pub(super) fn start_background(&mut self, index: usize) -> Result<u32, String> {
        let script = &self.scripts.list[index];
        if let Some(run) = &script.run {
            return Err(format!(
                "{} is already running (pid {})",
                script.found.title(),
                run.pid
            ));
        }
        let name = script.found.name.clone();

        let mut command = Command::new(&script.found.path);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group, so stopping it also stops whatever it started.
            .process_group(0)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("TESSERA_SCRIPT", &name)
            .env_remove("DISPLAY");
        if let Some(ipc) = &self.ipc {
            command.env(tessera_ipc::SOCKET_ENV, ipc.path());
        }
        if let Some(home) = std::env::var_os("HOME") {
            command.current_dir(home);
        }

        let mut child = command
            .spawn()
            .map_err(|err| format!("could not run {}: {err}", script.found.path.display()))?;
        let pid = child.id();

        // The pidfd is taken before anything could reap the child, so it
        // always refers to this process and not a recycled pid.
        let pidfd = Pid::from_raw(pid as i32)
            .ok_or_else(|| "the script has no process id".to_string())
            .and_then(|raw| {
                pidfd_open(raw, PidfdFlags::empty())
                    .map_err(|err| format!("cannot watch the script's process: {err}"))
            });
        let pidfd = match pidfd {
            Ok(fd) => fd,
            Err(message) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(message);
            }
        };

        let run_id = self.next_run_id();
        let mut pipes = Vec::new();
        let outputs = [
            (false, child.stdout.take().map(OwnedFd::from)),
            (true, child.stderr.take().map(OwnedFd::from)),
        ];
        for (stderr, fd) in outputs {
            let Some(fd) = fd else { continue };
            let file = File::from(fd);
            if let Err(err) = set_nonblocking(&file) {
                tracing::warn!(%err, "cannot make a script pipe non-blocking");
                continue;
            }
            if let Ok(second) = file.try_clone() {
                pipes.push((stderr, second));
            }
            self.watch_pipe(&name, run_id, stderr, file);
        }

        let script = &mut self.scripts.list[index];
        script.output.clear();
        script.output_run = run_id;
        let started_at_ms = tessera_ipc::unix_millis();
        script.state = ScriptState::Running { pid, started_at_ms };
        script.run = Some(Run {
            id: run_id,
            child,
            pid,
            started: Instant::now(),
            started_at_ms,
            pipes,
        });

        let exit_name = name.clone();
        let inserted = self.loop_handle.insert_source(
            Generic::new(pidfd, Interest::READ, Mode::Level),
            move |_, _, state| {
                Ok(if state.script_process_ended(&exit_name, run_id) {
                    PostAction::Remove
                } else {
                    PostAction::Continue
                })
            },
        );
        if let Err(err) = inserted {
            // Without it the exit would never be noticed; better to say so now.
            tracing::warn!(error = %err.error, script = name, "cannot watch the script's exit");
        }

        tracing::info!(script = name, pid, "script started");
        Ok(pid)
    }

    /// Registers a pipe, after reading whatever is already waiting in it.
    ///
    /// The drain-first order is the S4 lesson: a source whose data (or EOF)
    /// was pending before registration may never be reported.
    fn watch_pipe(&mut self, name: &str, run_id: u64, stderr: bool, file: File) {
        if self.read_script_pipe(name, run_id, stderr, &file) == PostAction::Remove {
            return;
        }
        let name = name.to_string();
        let inserted = self.loop_handle.insert_source(
            Generic::new(file, Interest::READ, Mode::Level),
            move |_, file, state| Ok(state.read_script_pipe(&name, run_id, stderr, file)),
        );
        if let Err(err) = inserted {
            tracing::warn!(error = %err.error, "cannot watch a script pipe");
        }
    }

    /// Reads everything waiting in a pipe into the script's output.
    fn read_script_pipe(
        &mut self,
        name: &str,
        run_id: u64,
        stderr: bool,
        file: &File,
    ) -> PostAction {
        let mut reader: &File = file;
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    if let Some(script) = self.current_output(name, run_id) {
                        script.output.flush(stderr);
                    }
                    return PostAction::Remove;
                }
                Ok(count) => match self.current_output(name, run_id) {
                    Some(script) => script.output.push(stderr, &chunk[..count]),
                    // A newer run has started; this pipe belongs to the past.
                    None => return PostAction::Remove,
                },
                Err(err) if err.kind() == ErrorKind::WouldBlock => return PostAction::Continue,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => {
                    tracing::debug!(%err, "script pipe failed");
                    return PostAction::Remove;
                }
            }
        }
    }

    /// The script whose output `run_id` still owns.
    fn current_output(&mut self, name: &str, run_id: u64) -> Option<&mut super::Script> {
        self.scripts
            .list
            .iter_mut()
            .find(|script| script.found.name == name && script.output_run == run_id)
    }

    /// Called when a script's pidfd becomes readable. Returns true once the
    /// exit has been recorded and the pidfd can be dropped.
    fn script_process_ended(&mut self, name: &str, run_id: u64) -> bool {
        let Some(index) = self.script_index(name) else {
            return true;
        };
        let script = &mut self.scripts.list[index];
        let Some(run) = script.run.as_mut().filter(|run| run.id == run_id) else {
            return true;
        };
        let status = match run.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return false,
            Err(err) => {
                tracing::warn!(%err, script = name, "cannot read the script's exit status");
                return true;
            }
        };
        let exit = match (status.code(), status.signal()) {
            (Some(code), _) => ExitStatus::Code { code },
            (None, Some(signal)) => ExitStatus::Signal { signal },
            (None, None) => ExitStatus::Code { code: -1 },
        };

        let run = script.run.take().expect("checked above");
        let duration_ms = millis_since(run.started);
        // Whatever it printed just before exiting may still be in the pipes.
        for (stderr, file) in &run.pipes {
            self.read_script_pipe(name, run_id, *stderr, file);
        }
        let script = &mut self.scripts.list[index];
        script.output.flush(false);
        script.output.flush(true);
        script.state = ScriptState::Exited {
            exit,
            started_at_ms: run.started_at_ms,
            duration_ms,
        };

        tracing::info!(
            script = name,
            exit = exit.describe(),
            duration_ms,
            "script exited"
        );
        self.broadcast(&Event::ScriptExited {
            name: name.to_string(),
            exit,
            duration_ms,
        });
        true
    }

    /// Asks a script to stop, and kills it if it has not after [`STOP_GRACE`].
    pub(crate) fn stop_script(&mut self, index: usize) -> Result<(), String> {
        let script = &self.scripts.list[index];
        let Some(run) = &script.run else {
            return Err(format!("{} is not running", script.found.title()));
        };
        let (pid, run_id, name) = (run.pid, run.id, script.found.name.clone());
        signal_group(pid, Signal::TERM);

        let timer = Timer::from_duration(STOP_GRACE);
        let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
            let still_running = state
                .script_index(&name)
                .and_then(|index| state.scripts.list[index].run.as_ref())
                .is_some_and(|run| run.id == run_id);
            if still_running {
                tracing::info!(script = name, "script ignored SIGTERM; sending SIGKILL");
                signal_group(pid, Signal::KILL);
            }
            TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            tracing::warn!(error = %err.error, "cannot schedule SIGKILL; killing now");
            signal_group(pid, Signal::KILL);
        }
        Ok(())
    }

    /// Asks every running script to stop, when the compositor exits.
    pub fn stop_all_scripts(&mut self) {
        for script in &self.scripts.list {
            if let Some(run) = &script.run {
                signal_group(run.pid, Signal::TERM);
            }
        }
    }
}

/// Signals a script's whole process group.
fn signal_group(pid: u32, signal: Signal) {
    let Some(pid) = Pid::from_raw(pid as i32) else {
        return;
    };
    if let Err(err) = kill_process_group(pid, signal) {
        // ESRCH: everything in the group has already gone.
        tracing::debug!(%err, "signalling a script's process group failed");
    }
}

fn set_nonblocking(file: &File) -> rustix::io::Result<()> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let flags = fcntl_getfl(file)?;
    fcntl_setfl(file, flags | OFlags::NONBLOCK)
}
