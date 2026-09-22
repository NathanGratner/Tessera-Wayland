//! Running `systemctl`, and reading what it says.
//!
//! Every call here blocks until systemctl returns, which for a start or stop
//! can take as long as the service does. The launcher therefore calls these
//! from a worker thread, never from its event loop.

use std::process::{Command, Output, Stdio};

use crate::{Scope, Service};

/// The properties asked for in one batch; see [`UnitState`].
const PROPERTIES: &str = "Id,Description,LoadState,ActiveState,SubState,UnitFileState";

/// What systemd says about a unit right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitState {
    /// The unit's own name.
    pub id: String,
    /// Its one-line description.
    pub description: String,
    /// `loaded`, `not-found`, `masked`, …
    pub load: String,
    /// `active`, `inactive`, `failed`, `activating`, `deactivating`, `reloading`.
    pub active: String,
    /// Finer detail: `running`, `exited`, `dead`, `waiting`, …
    pub sub: String,
    /// `enabled`, `disabled`, `static`, `masked`, … (empty when not found).
    pub enabled: String,
}

impl UnitState {
    /// Whether it is running, or on its way there.
    pub fn is_active(&self) -> bool {
        matches!(self.active.as_str(), "active" | "activating" | "reloading")
    }

    /// Whether it failed last time it ran.
    pub fn is_failed(&self) -> bool {
        self.active == "failed"
    }

    /// Whether systemd knows the unit at all.
    pub fn exists(&self) -> bool {
        self.load != "not-found"
    }

    /// Whether it starts at boot (or login, for user units).
    pub fn is_enabled(&self) -> bool {
        matches!(
            self.enabled.as_str(),
            "enabled" | "enabled-runtime" | "alias"
        )
    }

    /// Whether `enable` and `disable` mean anything for it. Static units are
    /// only started by something else, and masked ones cannot be started at all.
    pub fn can_toggle_enabled(&self) -> bool {
        matches!(
            self.enabled.as_str(),
            "enabled" | "enabled-runtime" | "disabled"
        )
    }
}

/// Parses `systemctl show -p …` output: `Key=value` lines, one blank line
/// between units, units in the order they were asked for.
pub fn parse_show(text: &str) -> Vec<UnitState> {
    let mut units = Vec::new();
    let mut current: Option<UnitState> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            units.extend(current.take());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let unit = current.get_or_insert_with(UnitState::default);
        let value = value.to_string();
        match key {
            "Id" => unit.id = value,
            "Description" => unit.description = value,
            "LoadState" => unit.load = value,
            "ActiveState" => unit.active = value,
            "SubState" => unit.sub = value,
            "UnitFileState" => unit.enabled = value,
            _ => {}
        }
    }
    units.extend(current);
    units
}

/// `systemctl`, or `systemctl --user`.
pub fn systemctl(scope: Scope) -> Command {
    let mut command = Command::new("systemctl");
    if scope == Scope::User {
        command.arg("--user");
    }
    command
}

/// The current state of each service, in the order given. `None` where
/// systemctl could not be asked (not installed, no user manager).
pub fn query(services: &[Service]) -> Vec<Option<UnitState>> {
    let mut states = vec![None; services.len()];
    for scope in [Scope::System, Scope::User] {
        let indices: Vec<usize> = (0..services.len())
            .filter(|&index| services[index].scope == scope)
            .collect();
        if indices.is_empty() {
            continue;
        }
        let output = systemctl(scope)
            .args(["show", "--no-pager", "-p", PROPERTIES, "--"])
            .args(indices.iter().map(|&index| &services[index].unit))
            .stdin(Stdio::null())
            .output();
        let Ok(output) = output else {
            continue;
        };
        let units = parse_show(&String::from_utf8_lossy(&output.stdout));
        // systemctl answers in the order asked; a count mismatch means it did
        // not answer for every unit, and the pairing cannot be trusted.
        if units.len() == indices.len() {
            for (index, unit) in indices.into_iter().zip(units) {
                states[index] = Some(unit);
            }
        }
    }
    states
}

/// Something systemctl can be asked to do to a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Start it now.
    Start,
    /// Stop it now.
    Stop,
    /// Stop and start it.
    Restart,
    /// Start it at boot.
    Enable,
    /// Stop starting it at boot.
    Disable,
}

impl Verb {
    /// The systemctl subcommand.
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Stop => "stop",
            Verb::Restart => "restart",
            Verb::Enable => "enable",
            Verb::Disable => "disable",
        }
    }

    /// Parses a subcommand name.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "start" => Verb::Start,
            "stop" => Verb::Stop,
            "restart" => Verb::Restart,
            "enable" => Verb::Enable,
            "disable" => Verb::Disable,
            _ => return None,
        })
    }

    /// Past tense, for status lines: "started sshd.service".
    pub fn done(self) -> &'static str {
        match self {
            Verb::Start => "started",
            Verb::Stop => "stopped",
            Verb::Restart => "restarted",
            Verb::Enable => "enabled",
            Verb::Disable => "disabled",
        }
    }
}

/// Why an action did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// systemd wants a password; nothing was changed. Run it again somewhere
    /// that can ask, such as a terminal.
    NeedsAuth,
    /// It failed for some other reason, which systemctl explained.
    Failed(String),
}

/// Runs `systemctl <verb> <unit>` without ever prompting.
///
/// `--no-ask-password` makes systemd refuse instead of asking a polkit agent,
/// which is reported as [`ActionError::NeedsAuth`] so the caller can retry in
/// a terminal where systemctl's own agent asks for the password.
pub fn act(verb: Verb, service: &Service) -> Result<(), ActionError> {
    let output = systemctl(service.scope)
        .args(["--no-ask-password", verb.as_str(), "--", &service.unit])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| ActionError::Failed(format!("cannot run systemctl: {err}")))?;
    classify(verb, service, &output)
}

fn classify(verb: Verb, service: &Service, output: &Output) -> Result<(), ActionError> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if needs_auth(&stderr) {
        return Err(ActionError::NeedsAuth);
    }
    let reason = stderr.lines().find(|line| !line.trim().is_empty());
    Err(ActionError::Failed(match reason {
        Some(reason) => reason.trim().to_string(),
        None => format!("systemctl {} {} failed", verb.as_str(), service.unit),
    }))
}

/// Something the whole machine can be asked to do, through logind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    /// Sleep, keeping everything in memory.
    Suspend,
    /// Restart the computer.
    Reboot,
    /// Turn the computer off.
    PowerOff,
}

impl PowerAction {
    /// The systemctl subcommand.
    pub fn as_str(self) -> &'static str {
        match self {
            PowerAction::Suspend => "suspend",
            PowerAction::Reboot => "reboot",
            PowerAction::PowerOff => "poweroff",
        }
    }
}

/// Runs `systemctl suspend`, `reboot` or `poweroff` without ever prompting.
///
/// logind lets the user of the active local session do these without a
/// password, unless someone else is logged in too or the local polkit rules
/// say otherwise; then this reports [`ActionError::NeedsAuth`], and the
/// caller can run the same command in a terminal, where systemctl asks.
pub fn power(action: PowerAction) -> Result<(), ActionError> {
    let output = Command::new("systemctl")
        .args(["--no-ask-password", action.as_str()])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| ActionError::Failed(format!("cannot run systemctl: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if needs_auth(&stderr) {
        return Err(ActionError::NeedsAuth);
    }
    Err(ActionError::Failed(
        stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .unwrap_or_else(|| format!("systemctl {} failed", action.as_str())),
    ))
}

/// Whether systemctl's error means "a password is needed".
///
/// With `--no-ask-password` it says `Interactive authentication required.`;
/// some versions and polkit setups say `Access denied` instead.
pub fn needs_auth(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("interactive authentication required") || lower.contains("access denied")
}

/// `systemctl status` for one unit, as lines, oldest journal line last.
///
/// systemctl exits with 3 for an inactive unit, which is still a perfectly
/// good status; only "no such unit" (4) and failing to run count as errors.
pub fn status(service: &Service, journal_lines: u32) -> Result<Vec<String>, String> {
    let output = systemctl(service.scope)
        .args([
            "status",
            "--no-pager",
            "--full",
            &format!("--lines={journal_lines}"),
            "--",
            &service.unit,
        ])
        .env("SYSTEMD_COLORS", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("cannot run systemctl: {err}"))?;
    if output.status.code() == Some(4) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from `systemctl show -p … sshd bluetooth.service nosuchthing.service`.
    const SHOW: &str = "\
Id=sshd.service
Description=OpenSSH Daemon
LoadState=loaded
ActiveState=active
SubState=running
UnitFileState=enabled

Id=bluetooth.service
Description=Bluetooth service
LoadState=loaded
ActiveState=inactive
SubState=dead
UnitFileState=disabled

Id=nosuchthing.service
Description=nosuchthing.service
LoadState=not-found
ActiveState=inactive
SubState=dead
UnitFileState=
";

    #[test]
    fn show_output_parses_into_one_state_per_unit() {
        let units = parse_show(SHOW);
        assert_eq!(units.len(), 3);
        assert_eq!(units[0].id, "sshd.service");
        assert_eq!(units[0].description, "OpenSSH Daemon");
        assert!(units[0].is_active());
        assert!(units[0].is_enabled());
        assert!(!units[1].is_active());
        assert!(!units[1].is_enabled());
        assert!(units[1].can_toggle_enabled());
        assert!(!units[2].exists());
        assert!(!units[2].can_toggle_enabled());
    }

    #[test]
    fn values_may_contain_equals_signs() {
        let units = parse_show("Id=a.service\nDescription=x=y\n");
        assert_eq!(units[0].description, "x=y");
    }

    #[test]
    fn static_units_cannot_be_enabled_or_disabled() {
        let unit = UnitState {
            enabled: "static".into(),
            ..Default::default()
        };
        assert!(!unit.can_toggle_enabled());
        assert!(!unit.is_enabled());
    }

    #[test]
    fn authentication_failures_are_recognised() {
        assert!(needs_auth(
            "Failed to start sshd.service: Interactive authentication required.\nSee system logs."
        ));
        assert!(needs_auth("Failed to stop x.service: Access denied"));
        assert!(!needs_auth(
            "Failed to start x.service: Unit x.service not found."
        ));
    }

    #[test]
    fn power_actions_name_their_subcommands() {
        assert_eq!(PowerAction::Suspend.as_str(), "suspend");
        assert_eq!(PowerAction::Reboot.as_str(), "reboot");
        assert_eq!(PowerAction::PowerOff.as_str(), "poweroff");
    }

    #[test]
    fn verbs_round_trip() {
        for verb in [
            Verb::Start,
            Verb::Stop,
            Verb::Restart,
            Verb::Enable,
            Verb::Disable,
        ] {
            assert_eq!(Verb::parse(verb.as_str()), Some(verb));
        }
        assert_eq!(Verb::parse("mask"), None);
    }
}
