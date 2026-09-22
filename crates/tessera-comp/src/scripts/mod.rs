//! User scripts: finding them, running them, and remembering what they said (design §7).
//!
//! The compositor owns scripts, not the launcher, so key bindings and
//! autostart work while the launcher is closed, and a script started from the
//! menu outlives the menu. The launcher only displays what is here, over IPC.
//!
//! - [`discover`] scans the folder and reads headers.
//! - [`watch`] notices changes to the folder and rescans.
//! - [`supervisor`] runs background scripts, captures their output into an
//!   [`output::OutputBuffer`] and learns about exits through a pidfd.

pub mod discover;
pub mod output;
mod supervisor;
mod watch;

use std::{path::PathBuf, time::Instant};

use tessera_config::scripts::{Mode, scripts_dir};
use tessera_ipc::{Event, OutputLine, Placement, RunMode, ScriptInfo, ScriptState};

use self::{discover::Discovered, output::OutputBuffer};
use crate::{
    input::{Bindings, ScriptBinding},
    state::Tessera,
};

/// One script and everything the compositor knows about it.
#[derive(Debug)]
pub struct Script {
    /// What the folder scan found: name, path, header.
    pub found: Discovered,
    /// The binding in effect, once clashes are resolved.
    pub bind: Option<String>,
    /// Header problems and binding clashes, for the launcher to show.
    pub problems: Vec<String>,
    /// Running, finished, or never run.
    pub state: ScriptState,
    /// Output of the most recent background run.
    pub output: OutputBuffer,
    /// Which run the output belongs to, so a straggling pipe from an older
    /// run cannot write into a newer run's output.
    pub output_run: u64,
    /// The process, while it runs in the background.
    pub run: Option<supervisor::Run>,
}

impl Script {
    fn new(found: Discovered) -> Self {
        Self {
            found,
            bind: None,
            problems: Vec::new(),
            state: ScriptState::NeverRun,
            output: OutputBuffer::default(),
            output_run: 0,
            run: None,
        }
    }

    fn info(&self) -> ScriptInfo {
        let header = &self.found.header;
        ScriptInfo {
            name: self.found.name.clone(),
            title: self.found.title().to_string(),
            description: header.description.clone(),
            path: self.found.path.display().to_string(),
            mode: match header.mode {
                Mode::Background => RunMode::Background,
                Mode::Terminal => RunMode::Terminal,
            },
            bind: self.bind.clone(),
            autostart: header.autostart,
            problems: self.problems.clone(),
            state: self.state,
        }
    }
}

/// The compositor's script list.
#[derive(Debug, Default)]
pub struct Scripts {
    /// The folder being watched.
    pub dir: PathBuf,
    /// Sorted by title, as the launcher shows them.
    pub list: Vec<Script>,
    /// Hands out run ids.
    next_run_id: u64,
    /// A rescan timer is already waiting.
    rescan_pending: bool,
}

impl Tessera {
    /// Finds the scripts, starts watching the folder, and runs the autostart ones.
    ///
    /// Called once the IPC socket exists, so scripts inherit `TESSERA_SOCKET`.
    pub fn init_scripts(&mut self) {
        let dir = scripts_dir();
        // Creating it gives people somewhere to put scripts, and something to watch.
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::warn!(%err, dir = %dir.display(), "cannot create the scripts folder");
        }
        if let Err(err) = watch::watch(&self.loop_handle, &dir) {
            tracing::warn!(error = %format!("{err:#}"), "not watching the scripts folder; new scripts need a restart");
        }
        self.scripts.dir = dir;
        self.rescan_scripts();

        let autostart: Vec<usize> = (0..self.scripts.list.len())
            .filter(|&index| self.scripts.list[index].found.header.autostart)
            .collect();
        for index in autostart {
            self.run_script_at(index);
        }
        tracing::info!(
            dir = %self.scripts.dir.display(),
            count = self.scripts.list.len(),
            "scripts ready"
        );
    }

    /// Re-reads the folder, keeping what is known about scripts that are still there.
    pub(crate) fn rescan_scripts(&mut self) {
        let found = discover::scan(&self.scripts.dir);
        let mut old = std::mem::take(&mut self.scripts.list);

        let mut list: Vec<Script> = found
            .into_iter()
            .map(
                |found| match old.iter().position(|s| s.found.name == found.name) {
                    Some(index) => {
                        let mut script = old.swap_remove(index);
                        script.found = found;
                        script
                    }
                    None => Script::new(found),
                },
            )
            .collect();
        // A script deleted while it runs stays listed until it exits, so it can still be stopped.
        list.extend(old.into_iter().filter(|script| script.run.is_some()));

        self.scripts.list = list;
        self.rebuild_bindings();
        self.broadcast(&Event::ScriptsChanged);
    }

    /// Builds the binding table from the configuration plus the scripts' headers.
    pub(crate) fn rebuild_bindings(&mut self) {
        // The settings in effect were validated when they were applied.
        let mut bindings = Bindings::from_config(&self.settings).unwrap_or_default();
        let wanted: Vec<ScriptBinding<'_>> = self
            .scripts
            .list
            .iter()
            .enumerate()
            .filter_map(|(index, script)| {
                Some(ScriptBinding {
                    index,
                    name: &script.found.name,
                    text: script.found.header.bind.as_deref()?,
                })
            })
            .collect();
        let binding_problems = bindings.add_scripts(&wanted);

        for (index, script) in self.scripts.list.iter_mut().enumerate() {
            script.problems = script.found.header.problems.clone();
            let refused: Vec<String> = binding_problems
                .iter()
                .filter(|(owner, _)| *owner == index)
                .map(|(_, problem)| problem.clone())
                .collect();
            script.bind = if refused.is_empty() {
                script.found.header.bind.clone()
            } else {
                None
            };
            for problem in &refused {
                tracing::warn!(
                    script = script.found.name,
                    problem,
                    "script binding skipped"
                );
            }
            script.problems.extend(refused);
        }
        self.bindings = bindings;
    }

    /// Runs a script from a key binding or autostart, logging any refusal.
    pub(crate) fn run_script_at(&mut self, index: usize) {
        if let Err(message) = self.run_script(index, None, None) {
            tracing::warn!(message, "could not run a script");
        }
    }

    /// Runs a script in `mode`, or the mode its header asks for.
    ///
    /// `caller_pid` is the IPC peer, so a terminal-mode script started from the
    /// launcher opens beside the launcher. Returns the process id.
    pub(crate) fn run_script(
        &mut self,
        index: usize,
        mode: Option<RunMode>,
        caller_pid: Option<u32>,
    ) -> Result<u32, String> {
        let script = self.scripts.list.get(index).ok_or("no such script")?;
        let mode = mode.unwrap_or(match script.found.header.mode {
            Mode::Background => RunMode::Background,
            Mode::Terminal => RunMode::Terminal,
        });
        let name = script.found.name.clone();
        let pid = match mode {
            RunMode::Background => self.start_background(index)?,
            RunMode::Terminal => self.start_in_terminal(index, caller_pid)?,
        };
        self.broadcast(&Event::ScriptStarted { name, pid });
        Ok(pid)
    }

    /// Opens a script in a terminal tiled beside the caller, the §8 path.
    fn start_in_terminal(&mut self, index: usize, caller_pid: Option<u32>) -> Result<u32, String> {
        let path = self.scripts.list[index].found.path.display().to_string();
        let mut argv = self.behaviour.terminal.clone();
        argv.extend(["-e".to_string(), path]);
        // From the launcher: beside the caller. From a key binding there is no
        // caller, so it goes where Mod+Return would put a terminal.
        let spawned = match caller_pid {
            Some(_) => {
                let placement = Placement::BesideCaller {
                    side: self.terminal_side(),
                    ratio: self.beside_share(),
                };
                self.spawn_placed(&argv, placement, caller_pid)
            }
            None => {
                let kind = self.beside_launcher_or_auto();
                self.spawn_with(&argv, kind)
            }
        };
        let pid = spawned.map_err(|err| format!("{err:#}"))?;
        self.scripts.list[index].state = ScriptState::InTerminal {
            started_at_ms: tessera_ipc::unix_millis(),
        };
        Ok(pid)
    }

    /// The position of a script in the list, by file name.
    pub(crate) fn script_index(&self, name: &str) -> Option<usize> {
        self.scripts
            .list
            .iter()
            .position(|script| script.found.name == name)
    }

    /// Everything the launcher shows about every script.
    pub(crate) fn script_infos(&self) -> Vec<ScriptInfo> {
        self.scripts.list.iter().map(Script::info).collect()
    }

    /// A script's latest output, and whether more may follow.
    pub(crate) fn script_output(&self, index: usize, tail: usize) -> (Vec<OutputLine>, bool) {
        let script = &self.scripts.list[index];
        (script.output.tail(tail), script.run.is_some())
    }

    /// Asks for a rescan shortly, so a burst of changes (an editor saving)
    /// costs one scan rather than a dozen.
    pub(crate) fn schedule_rescan(&mut self) {
        if self.scripts.rescan_pending {
            return;
        }
        self.scripts.rescan_pending = true;
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(
            std::time::Duration::from_millis(200),
        );
        let inserted = self.loop_handle.insert_source(timer, |_, _, state| {
            state.scripts.rescan_pending = false;
            state.rescan_scripts();
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(err) = inserted {
            tracing::warn!(error = %err.error, "cannot schedule a script rescan; rescanning now");
            self.scripts.rescan_pending = false;
            self.rescan_scripts();
        }
    }

    /// Next run id; never zero, so zero can mean "no run yet".
    fn next_run_id(&mut self) -> u64 {
        self.scripts.next_run_id += 1;
        self.scripts.next_run_id
    }
}

/// Milliseconds elapsed since `start`.
fn millis_since(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}
