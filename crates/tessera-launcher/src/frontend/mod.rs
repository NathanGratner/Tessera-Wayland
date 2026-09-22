//! Front ends: a plain terminal, or a Wayland window drawn cell by cell.

pub mod tty;
pub mod wayland;

use std::process::Command;

use tessera_config::{ConfigValues, config_path};
use tessera_ipc::{Request, Response, RunMode, ScriptInfo, WindowInfo};

use crate::{
    app::App, app::Effect, config_menu::SaveReport, ipc::Ipc, scripts_menu::OutputResult,
    worker::Worker,
};

/// Results the app is waiting for, handed back after the effects ran.
#[derive(Default)]
pub struct Feedback {
    windows: Option<Vec<WindowInfo>>,
    saved: Option<Result<SaveReport, String>>,
    loaded: Option<Result<ConfigValues, String>>,
    scripts: Option<Result<Vec<ScriptInfo>, String>>,
    output: Option<(String, OutputResult)>,
    report: Option<Result<String, String>>,
    session: Option<Result<(), String>>,
}

impl Feedback {
    /// Gives each result to the part of the app that asked for it.
    pub fn deliver(self, app: &mut App) {
        if let Some(windows) = self.windows {
            app.set_windows(windows);
        }
        if let Some(result) = self.saved {
            app.config_saved(result);
        }
        if let Some(result) = self.loaded {
            app.config_loaded(result);
        }
        if let Some(result) = self.scripts {
            app.scripts_listed(result);
        }
        if let Some((name, result)) = self.output {
            app.script_output(&name, result);
        }
        if let Some(result) = self.report {
            app.script_report(result);
        }
        if let Some(result) = self.session {
            app.session_report(result);
        }
    }
}

/// Carries out the effects an update produced.
///
/// Under Tessera this asks the compositor to start the program, so it can place
/// the window. Elsewhere (a plain terminal, or another compositor) the launcher
/// runs it directly and the window lands wherever that compositor puts it.
/// Returns any results the app is waiting for; pass them on with
/// [`Feedback::deliver`].
///
/// Requests to the compositor answer at once and run here. Anything that
/// calls `systemctl` can block for seconds, so it goes to `worker`, whose
/// results arrive later through [`App::background`].
pub fn run_effects(effects: &[Effect], ipc: Option<&Ipc>, worker: &Worker) -> Feedback {
    let mut feedback = Feedback::default();
    for effect in effects {
        match effect {
            Effect::ListScripts => {
                feedback.scripts = Some(match ask(ipc, &Request::ListScripts) {
                    Ok(Response::Scripts { scripts }) => Ok(scripts),
                    Ok(other) => Err(unexpected(&other)),
                    Err(message) => Err(message),
                });
            }
            Effect::RunScript { name, mode } => {
                let request = Request::RunScript {
                    name: name.clone(),
                    mode: *mode,
                };
                feedback.report = Some(match ask(ipc, &request) {
                    Ok(Response::Spawned { .. }) if *mode == Some(RunMode::Terminal) => {
                        Ok(format!("opened {name} in a terminal"))
                    }
                    Ok(Response::Spawned { pid }) => Ok(format!("started {name} (pid {pid})")),
                    Ok(other) => Err(unexpected(&other)),
                    Err(message) => Err(message),
                });
            }
            Effect::StopScript { name } => {
                let request = Request::StopScript { name: name.clone() };
                feedback.report = Some(match ask(ipc, &request) {
                    Ok(Response::Ok) => Ok(format!("stopping {name}…")),
                    Ok(other) => Err(unexpected(&other)),
                    Err(message) => Err(message),
                });
            }
            Effect::ScriptOutput { name } => {
                let request = Request::ScriptOutput {
                    name: name.clone(),
                    tail: 500,
                };
                let result = match ask(ipc, &request) {
                    Ok(Response::ScriptOutput { lines, running, .. }) => Ok((lines, running)),
                    Ok(other) => Err(unexpected(&other)),
                    Err(message) => Err(message),
                };
                feedback.output = Some((name.clone(), result));
            }
            Effect::SetAutostart { path, on } => {
                feedback.report = Some(set_autostart(path, *on));
            }
            Effect::EndSession => {
                feedback.session = Some(match ask(ipc, &Request::Quit) {
                    Ok(Response::Ok) => Ok(()),
                    Ok(other) => Err(unexpected(&other)),
                    Err(message) => Err(message),
                });
            }
            Effect::Power(action) => worker.power(*action),
            Effect::QueryServices => worker.query_services(),
            Effect::ServiceAction { service, verb } => {
                worker.service_action(service.clone(), *verb)
            }
            Effect::ServiceStatus { service } => worker.service_status(service.clone()),
            Effect::AddService { service } => worker.add_service(service.clone()),
            Effect::RemoveService { service } => worker.remove_service(service.clone()),
            Effect::ListWindows => feedback.windows = Some(list_windows(ipc)),
            Effect::Focus(id) => focus_window(ipc, *id),
            Effect::Spawn { argv, placement } => spawn(ipc, argv, *placement),
            Effect::SaveConfig { toml } => feedback.saved = Some(save_config(ipc, toml)),
            Effect::LoadConfig => {
                feedback.loaded =
                    Some(ConfigValues::load(&config_path()).map_err(|err| err.to_string()));
            }
            Effect::Quit => {}
        }
    }
    feedback
}

/// Sends a request, turning every kind of failure into a message.
fn ask(ipc: Option<&Ipc>, request: &Request) -> Result<Response, String> {
    let ipc = ipc.ok_or("not running under Tessera")?;
    match ipc.request(request) {
        Ok(Response::Error { message }) => Err(message),
        Ok(response) => Ok(response),
        Err(err) => Err(format!("could not reach the compositor: {err}")),
    }
}

fn unexpected(response: &Response) -> String {
    format!("the compositor answered {response:?}")
}

/// Rewrites a script's `autostart` header. The compositor notices the file
/// change through its folder watch, so nothing needs to be sent to it.
fn set_autostart(path: &str, on: bool) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("cannot read {path}: {err}"))?;
    let updated = tessera_config::scripts::set_autostart(&text, on);
    // Writing in place keeps the file's permissions, including the executable bit.
    std::fs::write(path, updated).map_err(|err| format!("cannot write {path}: {err}"))?;
    let name = std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    Ok(if on {
        format!("{name} will start with the session")
    } else {
        format!("{name} will no longer start with the session")
    })
}

/// Writes config.toml, then asks the compositor to apply it.
fn save_config(ipc: Option<&Ipc>, toml: &str) -> Result<SaveReport, String> {
    let values = ConfigValues::parse(toml).map_err(|err| err.to_string())?;
    values.save(&config_path()).map_err(|err| err.to_string())?;

    let Some(ipc) = ipc else {
        return Ok(SaveReport {
            live: Vec::new(),
            needs_restart: Vec::new(),
            reached_compositor: false,
        });
    };
    match ipc.request(&Request::ReloadConfig) {
        Ok(Response::ConfigApplied {
            live,
            needs_restart,
        }) => Ok(SaveReport {
            live,
            needs_restart,
            reached_compositor: true,
        }),
        // The file is saved; the compositor refused it, and says why.
        Ok(Response::Error { message }) => Err(format!("saved, but not applied: {message}")),
        Ok(other) => Err(format!("saved, but the compositor answered {other:?}")),
        Err(err) => Err(format!(
            "saved, but the compositor could not be reached: {err}"
        )),
    }
}

/// Asks the compositor which windows exist. Empty when it cannot be reached.
fn list_windows(ipc: Option<&Ipc>) -> Vec<WindowInfo> {
    let Some(ipc) = ipc else {
        return Vec::new();
    };
    match ipc.request(&Request::ListWindows) {
        Ok(Response::Windows { windows }) => windows,
        Ok(Response::Error { message }) => {
            tracing::warn!(message, "could not list windows");
            Vec::new()
        }
        Ok(other) => {
            tracing::warn!(?other, "unexpected response to ListWindows");
            Vec::new()
        }
        Err(err) => {
            tracing::warn!(%err, "could not reach the compositor");
            Vec::new()
        }
    }
}

fn focus_window(ipc: Option<&Ipc>, id: tessera_ipc::WindowId) {
    let Some(ipc) = ipc else {
        tracing::warn!("cannot switch windows without a compositor");
        return;
    };
    match ipc.request(&Request::Focus { window: id }) {
        Ok(Response::Ok) => tracing::info!(id = id.0, "focused"),
        Ok(Response::Error { message }) => tracing::warn!(message, "could not focus"),
        Ok(other) => tracing::warn!(?other, "unexpected response to Focus"),
        Err(err) => tracing::warn!(%err, "could not reach the compositor"),
    }
}

fn spawn(ipc: Option<&Ipc>, argv: &[String], placement: tessera_ipc::Placement) {
    {
        if argv.is_empty() {
            return;
        }

        match ipc {
            Some(ipc) => {
                let request = Request::Spawn {
                    argv: argv.to_vec(),
                    placement,
                };
                match ipc.request(&request) {
                    Ok(Response::Spawned { pid }) => {
                        tracing::info!(pid, ?argv, "compositor started the program");
                    }
                    Ok(Response::Error { message }) => {
                        tracing::warn!(message, ?argv, "compositor refused to start the program");
                    }
                    Ok(other) => tracing::warn!(?other, "unexpected response"),
                    Err(err) => {
                        tracing::warn!(%err, "could not reach the compositor; starting it directly");
                        spawn_directly(argv);
                    }
                }
            }
            None => spawn_directly(argv),
        }
    }
}

fn spawn_directly(argv: &[String]) {
    match Command::new(&argv[0]).args(&argv[1..]).spawn() {
        Ok(mut child) => {
            tracing::info!(pid = child.id(), ?argv, "spawned");
            // Nothing waits on these, so reap them here rather than leaving zombies.
            std::thread::spawn(move || child.wait());
        }
        Err(err) => tracing::warn!(%err, ?argv, "failed to spawn"),
    }
}
