//! The Scripts & services screen (design §7, widened in S6).
//!
//! Two lists in one dialog. **Scripts** belong to the compositor, which runs
//! them; this screen asks for the list, shows it live from the compositor's
//! events, and sends run and stop requests. **Services** belong to systemd;
//! Tessera keeps a list of the ones worth showing (`services.toml`) and runs
//! `systemctl` for you on a worker thread, falling back to a terminal when
//! systemd wants a password.
//!
//! Like the configuration menu, this module never does I/O itself: keys and
//! background results go in, [`Effect`]s come out, so it is tested headlessly.

use std::cell::Cell;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, ListItem, Paragraph},
};
use tessera_ipc::{
    Event as IpcEvent, OutputLine, Placement, RunMode, ScriptInfo, ScriptState, Side,
};
use tessera_services::{ActionError, Scope, Service, UnitState, Verb};

use crate::{
    app::Effect,
    event::{Key, KeyCode},
    ui::{self, Tone},
    worker::Update,
};

/// A script's output lines and whether it is still running, or why they
/// could not be fetched.
pub type OutputResult = Result<(Vec<OutputLine>, bool), String>;

/// A service and what systemd last said about it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRow {
    /// The unit and its manager.
    pub service: Service,
    /// `None` when systemd could not be asked.
    pub state: Option<UnitState>,
}

/// What the screen needs to know about its surroundings, fixed when it opens.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// A compositor is there to run scripts.
    pub under_tessera: bool,
    /// The terminal command, for terminal-mode actions.
    pub terminal: Vec<String>,
    /// Share of the launcher's tile a terminal takes, 0.1–0.9.
    pub share: f32,
    /// Which side of the launcher terminals open on.
    pub side: Side,
    /// The editor command, split into words; empty when none is installed.
    pub editor: Vec<String>,
    /// How to run `tessera-serv`.
    pub serv: String,
    /// The scripts folder, for the heading.
    pub scripts_dir: String,
}

/// What a key did to the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Still here; carry out these effects.
    Stay(Vec<Effect>),
    /// Leave the screen.
    Close,
}

#[derive(Debug, Clone, PartialEq)]
enum Loaded<T> {
    Waiting,
    Ready(T),
    Failed(String),
}

/// Identifies the selected row across refreshes, which reorder and resize the list.
#[derive(Debug, Clone, PartialEq)]
enum Selected {
    Script(String),
    Service(Service),
}

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Heading(&'static str),
    Note(String),
    Script(usize),
    Service(usize),
}

impl Row {
    fn selectable(&self) -> bool {
        matches!(self, Row::Script(_) | Row::Service(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum LogSource {
    Script(String),
    Service(Service),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineTone {
    Normal,
    Error,
    Good,
    Dim,
}

#[derive(Debug, Clone, PartialEq)]
struct LogView {
    source: LogSource,
    title: String,
    lines: Vec<(String, LineTone)>,
    /// First visible line, when not following.
    top: usize,
    /// Stay pinned to the newest line.
    follow: bool,
    loaded: bool,
    running: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Layer {
    List,
    Log(LogView),
    Adding { text: String, error: Option<String> },
    Help,
}

/// The screen's state, kept between visits.
pub struct ScriptsMenu {
    ctx: Context,
    scripts: Loaded<Vec<ScriptInfo>>,
    services: Loaded<Vec<ServiceRow>>,
    /// A service query is on its way; don't start another.
    query_pending: bool,
    /// Actions sent to systemctl that have not come back.
    busy: Vec<(Service, Verb)>,
    selected: Option<Selected>,
    layer: Layer,
    message: Option<(String, Tone)>,
    ticks: u64,
    /// Height of the log box when last drawn, for paging.
    log_height: Cell<usize>,
}

const SCRIPT_HELP: [&str; 3] = [
    "<Enter> runs the script in the background.  <T> runs it in a tiled",
    "terminal.  <E> edits it, <O> shows its last output, <K> kills it.",
    "<A> toggles [*]: start with the session.  <N> adds a service.",
];
const SERVICE_HELP: [&str; 3] = [
    "<Enter> starts or stops the service.  <R> restarts it, <S> shows its",
    "status.  <A> toggles [*]: start at boot.  <N> adds a service, <D>",
    "removes it from this list.  A password is asked for in a terminal.",
];
const EMPTY_HELP: [&str; 3] = [
    "Scripts are the executables in your scripts folder.  Services are",
    "systemd units you add here with <N>, or with `tessera-serv enable`.",
    "<Esc> goes back.",
];
const HELP_TEXT: &str = "\
Scripts
  <Enter>, <R>   run in the background
  <T>            run in a terminal tiled beside the launcher
  <E>            edit in $EDITOR, in a tiled terminal
  <O>            show the output of the last run
  <K>            kill (SIGTERM, then SIGKILL after 3 seconds)
  <A>            toggle [*]: run when Tessera starts

Services
  <Enter>, <Space>  start or stop
  <R>            restart            <K>  stop
  <S>, <O>       show systemctl status, refreshed live
  <A>            toggle [*]: start at boot (systemctl enable)
  <N>            add a service      <D>  remove it from this list

When systemd needs a password, the action opens in a
terminal beside the launcher and asks there.";

impl ScriptsMenu {
    /// An empty screen; [`ScriptsMenu::open`] fills it.
    pub fn new() -> Self {
        Self {
            ctx: Context::default(),
            scripts: Loaded::Waiting,
            services: Loaded::Waiting,
            query_pending: false,
            busy: Vec::new(),
            selected: None,
            layer: Layer::List,
            message: None,
            ticks: 0,
            log_height: Cell::new(10),
        }
    }

    /// Shows the screen, asking for fresh lists.
    pub fn open(&mut self, ctx: Context) -> Vec<Effect> {
        self.ctx = ctx;
        self.layer = Layer::List;
        self.message = None;
        let mut effects = Vec::new();
        if self.ctx.under_tessera {
            effects.push(Effect::ListScripts);
        } else {
            self.scripts =
                Loaded::Failed("Scripts need the compositor: not running under Tessera.".into());
        }
        effects.extend(self.query_services());
        effects
    }

    fn query_services(&mut self) -> Option<Effect> {
        if self.query_pending {
            return None;
        }
        self.query_pending = true;
        Some(Effect::QueryServices)
    }

    // ---- results -------------------------------------------------------

    /// The compositor's script list arrived.
    pub fn set_scripts(&mut self, result: Result<Vec<ScriptInfo>, String>) {
        self.scripts = match result {
            Ok(scripts) => Loaded::Ready(scripts),
            Err(message) => Loaded::Failed(message),
        };
    }

    /// A script's output arrived.
    pub fn set_output(&mut self, name: &str, result: OutputResult) {
        let Layer::Log(log) = &mut self.layer else {
            return;
        };
        if log.source != LogSource::Script(name.to_string()) {
            return;
        }
        log.loaded = true;
        match result {
            Ok((lines, running)) => {
                log.running = running;
                log.error = None;
                log.lines = lines
                    .into_iter()
                    .map(|line| {
                        let tone = if line.stderr {
                            LineTone::Error
                        } else {
                            LineTone::Normal
                        };
                        (line.text, tone)
                    })
                    .collect();
            }
            Err(message) => log.error = Some(message),
        }
    }

    /// A request finished; show how it went.
    pub fn report(&mut self, result: Result<String, String>) {
        self.message = Some(match result {
            Ok(text) => (text, Tone::Info),
            Err(text) => (text, Tone::Error),
        });
    }

    /// Background work finished, or a second passed.
    pub fn update(&mut self, update: Update) -> Vec<Effect> {
        match update {
            // Not this screen's: the app hands power results to its own screen.
            Update::PowerDone { .. } => Vec::new(),
            Update::Tick => self.tick(),
            Update::Compositor(event) => {
                let mut effects = Vec::new();
                if matches!(
                    event,
                    IpcEvent::ScriptStarted { .. }
                        | IpcEvent::ScriptExited { .. }
                        | IpcEvent::ScriptsChanged
                ) && self.ctx.under_tessera
                {
                    effects.push(Effect::ListScripts);
                }
                // The last lines arrive around the exit; fetch once more.
                if let (IpcEvent::ScriptExited { name, .. }, Layer::Log(log)) =
                    (&event, &self.layer)
                    && log.source == LogSource::Script(name.clone())
                {
                    effects.push(Effect::ScriptOutput { name: name.clone() });
                }
                effects
            }
            Update::Services(result) => {
                self.query_pending = false;
                self.services = match result {
                    Ok(rows) => Loaded::Ready(rows),
                    Err(message) => Loaded::Failed(message),
                };
                Vec::new()
            }
            Update::ServiceDone {
                service,
                verb,
                result,
            } => {
                self.busy
                    .retain(|(busy, busy_verb)| !(busy == &service && *busy_verb == verb));
                let mut effects = Vec::new();
                match result {
                    Ok(()) => {
                        self.message = Some((format!("{} {service}", verb.done()), Tone::Info));
                    }
                    Err(ActionError::NeedsAuth) => {
                        self.message = Some((
                            format!(
                                "{} {service} needs a password: continue in the terminal",
                                verb.as_str()
                            ),
                            Tone::Info,
                        ));
                        effects.push(self.in_terminal(self.serv_argv(verb, &service)));
                    }
                    Err(ActionError::Failed(reason)) => {
                        self.message = Some((reason, Tone::Error));
                    }
                }
                effects.extend(self.query_services());
                if let Layer::Log(log) = &self.layer
                    && log.source == LogSource::Service(service.clone())
                {
                    effects.push(Effect::ServiceStatus { service });
                }
                effects
            }
            Update::ServiceStatus { service, result } => {
                let state = self.state_of(&service).cloned();
                if let Layer::Log(log) = &mut self.layer
                    && log.source == LogSource::Service(service.clone())
                {
                    log.loaded = true;
                    match result {
                        Ok(lines) => {
                            log.lines = colour_status(lines, state.as_ref());
                            log.error = None;
                        }
                        Err(message) => log.error = Some(message),
                    }
                }
                Vec::new()
            }
            Update::ServiceListChanged(result) => {
                self.report(result);
                self.query_pending = false;
                self.query_services().into_iter().collect()
            }
        }
    }

    /// Keeps timers moving and live views fresh.
    fn tick(&mut self) -> Vec<Effect> {
        self.ticks += 1;
        let every_other = self.ticks.is_multiple_of(2);
        match &self.layer {
            Layer::List if every_other => self.query_services().into_iter().collect(),
            Layer::Log(log) => match &log.source {
                LogSource::Script(name) if log.running || !log.loaded => {
                    vec![Effect::ScriptOutput { name: name.clone() }]
                }
                LogSource::Service(service) if every_other => {
                    let mut effects = vec![Effect::ServiceStatus {
                        service: service.clone(),
                    }];
                    effects.extend(self.query_services());
                    effects
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    // ---- keys ----------------------------------------------------------

    /// Handles a key press.
    pub fn key(&mut self, key: Key) -> Outcome {
        match &self.layer {
            Layer::Help => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ' | 'q' | 'b')
                ) {
                    self.layer = Layer::List;
                }
                Outcome::Stay(Vec::new())
            }
            Layer::Adding { .. } => Outcome::Stay(self.key_in_add(key)),
            Layer::Log(_) => {
                self.key_in_log(key);
                Outcome::Stay(Vec::new())
            }
            Layer::List => self.key_in_list(key),
        }
    }

    fn key_in_list(&mut self, key: Key) -> Outcome {
        let letter = match key.code {
            KeyCode::Char(ch) => Some(ch.to_ascii_lowercase()),
            _ => None,
        };
        // Letters act on the selected entry here (`K` stops, as in design §7),
        // so only the arrow keys move.
        match (key.code, letter) {
            (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::PageUp, _) => self.move_selection(-10),
            (KeyCode::PageDown, _) => self.move_selection(10),
            (KeyCode::Home, _) => self.move_selection(-1000),
            (KeyCode::End, _) => self.move_selection(1000),
            (KeyCode::Esc, _) | (_, Some('b' | 'q')) => return Outcome::Close,
            (_, Some('?' | 'h')) => self.layer = Layer::Help,
            (_, Some('n')) => {
                self.layer = Layer::Adding {
                    text: String::new(),
                    error: None,
                }
            }
            _ => {
                self.message = None;
                let effects = match self.selected_row() {
                    Some(Row::Script(index)) => self.script_key(index, key),
                    Some(Row::Service(index)) => self.service_key(index, key),
                    _ => Vec::new(),
                };
                return Outcome::Stay(effects);
            }
        }
        Outcome::Stay(Vec::new())
    }

    fn script_key(&mut self, index: usize, key: Key) -> Vec<Effect> {
        let Loaded::Ready(scripts) = &self.scripts else {
            return Vec::new();
        };
        let script = scripts[index].clone();
        let letter = match key.code {
            KeyCode::Enter => 'r',
            KeyCode::Char(ch) => ch.to_ascii_lowercase(),
            _ => return Vec::new(),
        };
        match letter {
            'r' => vec![Effect::RunScript {
                name: script.name,
                mode: None,
            }],
            't' => vec![Effect::RunScript {
                name: script.name,
                mode: Some(RunMode::Terminal),
            }],
            'k' => vec![Effect::StopScript { name: script.name }],
            'e' if self.ctx.editor.is_empty() => {
                self.message = Some((
                    "No editor found: set $EDITOR, or install vim or nano".into(),
                    Tone::Error,
                ));
                Vec::new()
            }
            'e' => {
                let mut argv = self.ctx.editor.clone();
                argv.push(script.path.clone());
                vec![self.in_terminal(argv)]
            }
            'o' => {
                self.layer = Layer::Log(LogView {
                    source: LogSource::Script(script.name.clone()),
                    title: script.title.clone(),
                    lines: Vec::new(),
                    top: 0,
                    follow: true,
                    loaded: false,
                    running: matches!(script.state, ScriptState::Running { .. }),
                    error: None,
                });
                vec![Effect::ScriptOutput { name: script.name }]
            }
            'a' => vec![Effect::SetAutostart {
                path: script.path,
                on: !script.autostart,
            }],
            _ => Vec::new(),
        }
    }

    fn service_key(&mut self, index: usize, key: Key) -> Vec<Effect> {
        let Loaded::Ready(rows) = &self.services else {
            return Vec::new();
        };
        let row = rows[index].clone();
        let letter = match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => 'x',
            KeyCode::Char(ch) => ch.to_ascii_lowercase(),
            _ => return Vec::new(),
        };
        let active = row.state.as_ref().is_some_and(UnitState::is_active);
        let verb = match letter {
            'x' if active => Verb::Stop,
            'x' => Verb::Start,
            'r' => Verb::Restart,
            'k' => Verb::Stop,
            'a' => match &row.state {
                Some(state) if state.can_toggle_enabled() => {
                    if state.is_enabled() {
                        Verb::Disable
                    } else {
                        Verb::Enable
                    }
                }
                Some(state) if !state.exists() => {
                    self.message = Some((
                        format!("systemd has no unit called {}", row.service),
                        Tone::Error,
                    ));
                    return Vec::new();
                }
                Some(state) => {
                    self.message = Some((
                        format!(
                            "{} is {}, so it cannot be enabled or disabled",
                            row.service.unit, state.enabled
                        ),
                        Tone::Error,
                    ));
                    return Vec::new();
                }
                None => return Vec::new(),
            },
            's' | 'o' => {
                self.layer = Layer::Log(LogView {
                    source: LogSource::Service(row.service.clone()),
                    title: row.service.to_string(),
                    lines: Vec::new(),
                    top: 0,
                    follow: false,
                    loaded: false,
                    running: false,
                    error: None,
                });
                return vec![Effect::ServiceStatus {
                    service: row.service,
                }];
            }
            'd' => {
                return vec![Effect::RemoveService {
                    service: row.service,
                }];
            }
            _ => return Vec::new(),
        };
        if self.busy.iter().any(|(busy, _)| busy == &row.service) {
            self.message = Some((
                format!("still waiting for systemctl on {}", row.service.unit),
                Tone::Info,
            ));
            return Vec::new();
        }
        self.busy.push((row.service.clone(), verb));
        vec![Effect::ServiceAction {
            service: row.service,
            verb,
        }]
    }

    fn key_in_add(&mut self, key: Key) -> Vec<Effect> {
        let Layer::Adding { text, error } = &mut self.layer else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc => self.layer = Layer::List,
            KeyCode::Backspace => {
                text.pop();
                *error = None;
            }
            KeyCode::Enter => match parse_new_service(text) {
                Ok(service) => {
                    self.message = Some((format!("adding {service}…"), Tone::Info));
                    self.selected = Some(Selected::Service(service.clone()));
                    self.layer = Layer::List;
                    return vec![Effect::AddService { service }];
                }
                Err(reason) => *error = Some(reason),
            },
            _ => {
                if let Some(ch) = key.typed() {
                    text.push(ch);
                    *error = None;
                }
            }
        }
        Vec::new()
    }

    fn key_in_log(&mut self, key: Key) {
        let Layer::Log(log) = &mut self.layer else {
            return;
        };
        let height = self.log_height.get().max(1);
        let bottom = log.lines.len().saturating_sub(height);
        let top = if log.follow {
            bottom
        } else {
            log.top.min(bottom)
        };
        let (new_top, follow) = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'b') => {
                self.layer = Layer::List;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => (top.saturating_sub(1), false),
            KeyCode::Down | KeyCode::Char('j') => (top + 1, top + 1 >= bottom),
            KeyCode::PageUp => (top.saturating_sub(height), false),
            KeyCode::PageDown => (top + height, top + height >= bottom),
            KeyCode::Home | KeyCode::Char('g') => (0, false),
            KeyCode::End | KeyCode::Char('G') => (bottom, true),
            _ => return,
        };
        log.top = new_top.min(bottom);
        log.follow = follow;
    }

    fn move_selection(&mut self, delta: i32) {
        let rows = self.rows();
        let selectable: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].selectable()).collect();
        if selectable.is_empty() {
            return;
        }
        let current = self
            .selected_index(&rows)
            .and_then(|index| selectable.iter().position(|&i| i == index))
            .unwrap_or(0);
        let next = (current as i32 + delta).clamp(0, selectable.len() as i32 - 1) as usize;
        self.selected = self.key_for(&rows[selectable[next]]);
        self.message = None;
    }

    // ---- rows ----------------------------------------------------------

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Heading("Scripts")];
        match &self.scripts {
            Loaded::Waiting => rows.push(Row::Note("Asking the compositor…".into())),
            Loaded::Failed(message) => rows.push(Row::Note(message.clone())),
            Loaded::Ready(scripts) if scripts.is_empty() => rows.push(Row::Note(format!(
                "No scripts yet: put executables in {}",
                self.ctx.scripts_dir
            ))),
            Loaded::Ready(scripts) => rows.extend((0..scripts.len()).map(Row::Script)),
        }
        rows.push(Row::Heading("Services"));
        match &self.services {
            Loaded::Waiting => rows.push(Row::Note("Asking systemd…".into())),
            Loaded::Failed(message) => rows.push(Row::Note(message.clone())),
            Loaded::Ready(services) if services.is_empty() => rows.push(Row::Note(
                "None yet: <N> adds one, or run `tessera-serv enable <unit>`".into(),
            )),
            Loaded::Ready(services) => rows.extend((0..services.len()).map(Row::Service)),
        }
        rows
    }

    fn key_for(&self, row: &Row) -> Option<Selected> {
        match (row, &self.scripts, &self.services) {
            (Row::Script(index), Loaded::Ready(scripts), _) => {
                Some(Selected::Script(scripts[*index].name.clone()))
            }
            (Row::Service(index), _, Loaded::Ready(rows)) => {
                Some(Selected::Service(rows[*index].service.clone()))
            }
            _ => None,
        }
    }

    /// The selected row's index, falling back to the first selectable row.
    fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        let wanted = self.selected.as_ref();
        rows.iter()
            .position(|row| wanted.is_some() && self.key_for(row).as_ref() == wanted)
            .or_else(|| rows.iter().position(Row::selectable))
    }

    fn selected_row(&self) -> Option<Row> {
        let rows = self.rows();
        self.selected_index(&rows).map(|index| rows[index].clone())
    }

    fn state_of(&self, service: &Service) -> Option<&UnitState> {
        let Loaded::Ready(rows) = &self.services else {
            return None;
        };
        rows.iter()
            .find(|row| &row.service == service)
            .and_then(|row| row.state.as_ref())
    }

    // ---- effects -------------------------------------------------------

    /// Runs `argv` in a terminal tiled beside the launcher.
    fn in_terminal(&self, argv: Vec<String>) -> Effect {
        let mut command = self.ctx.terminal.clone();
        command.push("-e".into());
        command.extend(argv);
        Effect::Spawn {
            argv: command,
            placement: Placement::BesideCaller {
                side: self.ctx.side,
                ratio: self.ctx.share,
            },
        }
    }

    /// `tessera-serv <verb> [--user] --pause <unit>`, for the password fallback.
    fn serv_argv(&self, verb: Verb, service: &Service) -> Vec<String> {
        let mut argv = vec![self.ctx.serv.clone(), verb.as_str().to_string()];
        if service.scope == Scope::User {
            argv.push("--user".into());
        }
        argv.push("--pause".into());
        argv.push(service.unit.clone());
        argv
    }

    // ---- drawing -------------------------------------------------------

    /// Draws whichever layer is showing.
    pub fn view(&self, frame: &mut Frame) {
        match &self.layer {
            Layer::List => self.view_list(frame),
            Layer::Log(log) => self.view_log(frame, log),
            Layer::Adding { text, error } => self.view_add(frame, text, error.as_deref()),
            Layer::Help => ui::draw_note(frame, &self.heading(), "Scripts & services", HELP_TEXT),
        }
    }

    fn heading(&self) -> String {
        match &self.scripts {
            Loaded::Ready(scripts) => {
                let running = scripts
                    .iter()
                    .filter(|script| matches!(script.state, ScriptState::Running { .. }))
                    .count();
                let noun = if scripts.len() == 1 {
                    "script"
                } else {
                    "scripts"
                };
                format!(
                    "{} - {} {noun}, {running} running",
                    self.ctx.scripts_dir,
                    scripts.len()
                )
            }
            _ => self.ctx.scripts_dir.clone(),
        }
    }

    fn view_list(&self, frame: &mut Frame) {
        let area = ui::draw_screen(frame, &self.heading());
        let dialog = ui::centred(area, 78, area.height.saturating_sub(2).min(30));
        let inner = ui::draw_dialog(frame, dialog, "Scripts & services");
        let parts = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        let rows = self.rows();
        let selected = self.selected_index(&rows).unwrap_or(0);
        let selected_row = rows.get(selected);
        let help = match selected_row {
            Some(Row::Script(_)) => SCRIPT_HELP,
            Some(Row::Service(_)) => SERVICE_HELP,
            _ => EMPTY_HELP,
        };
        ui::draw_help(frame, parts[0], &help);

        let now = tessera_ipc::unix_millis();
        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let is_selected = index == selected;
                ListItem::new(match row {
                    Row::Heading(text) => Line::from(Span::styled(
                        format!(" {text}"),
                        ui::dialog_style()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Row::Note(text) => ui::menu_line_dim(&format!("    {text}"), false),
                    Row::Script(i) => {
                        let label = self.script_label(*i, now);
                        ui::menu_line(&label, None, is_selected)
                    }
                    Row::Service(i) => {
                        let label = self.service_label(*i);
                        ui::menu_line(&label, None, is_selected)
                    }
                })
            })
            .collect();

        let status = self.status_line(selected_row);
        ui::draw_list(
            frame,
            parts[1],
            items,
            selected,
            status.as_ref().map(|(text, tone)| (text.as_str(), *tone)),
        );

        let buttons: &[(&str, char)] = match selected_row {
            Some(Row::Script(_)) => &[
                ("Run", 'R'),
                ("In terminal", 'T'),
                ("Edit", 'E'),
                ("Output", 'O'),
                ("Kill", 'K'),
                ("Back", 'B'),
            ],
            Some(Row::Service(index)) => {
                if self.service_is_active(*index) {
                    &[
                        ("Stop", 'K'),
                        ("Restart", 'R'),
                        ("Status", 'S'),
                        ("New", 'N'),
                        ("Back", 'B'),
                    ]
                } else {
                    &[
                        ("Start", ' '),
                        ("Restart", 'R'),
                        ("Status", 'S'),
                        ("New", 'N'),
                        ("Back", 'B'),
                    ]
                }
            }
            _ => &[("New", 'N'), ("Back", 'B')],
        };
        ui::draw_buttons(frame, parts[2], buttons, 0);
    }

    fn service_is_active(&self, index: usize) -> bool {
        match &self.services {
            Loaded::Ready(rows) => rows[index].state.as_ref().is_some_and(UnitState::is_active),
            _ => false,
        }
    }

    /// The line over the list's bottom edge: a message, or what is wrong with
    /// the selected script, or what the selected service is.
    fn status_line(&self, row: Option<&Row>) -> Option<(String, Tone)> {
        if let Some(message) = &self.message {
            return Some(message.clone());
        }
        match (row, &self.scripts, &self.services) {
            (Some(Row::Script(index)), Loaded::Ready(scripts), _) => {
                let script = &scripts[*index];
                match script.problems.first() {
                    Some(problem) => Some((problem.clone(), Tone::Error)),
                    None => script
                        .description
                        .clone()
                        .map(|description| (description, Tone::Info)),
                }
            }
            (Some(Row::Service(index)), _, Loaded::Ready(rows)) => rows[*index]
                .state
                .as_ref()
                .filter(|state| state.exists())
                .map(|state| (state.description.clone(), Tone::Info)),
            _ => None,
        }
    }

    fn script_label(&self, index: usize, now: u64) -> String {
        let Loaded::Ready(scripts) = &self.scripts else {
            return String::new();
        };
        let script = &scripts[index];
        let marker = if script.autostart { "[*]" } else { "[ ]" };
        let (status, when) = match script.state {
            ScriptState::NeverRun => ("never run".to_string(), String::new()),
            ScriptState::Running { started_at_ms, .. } => (
                "running".to_string(),
                clock(now.saturating_sub(started_at_ms)),
            ),
            ScriptState::InTerminal { started_at_ms } => (
                "terminal".to_string(),
                ago(now.saturating_sub(started_at_ms)),
            ),
            ScriptState::Exited {
                exit,
                started_at_ms,
                duration_ms,
            } => (
                exit.describe(),
                ago(now.saturating_sub(started_at_ms + duration_ms)),
            ),
        };
        let warning = if script.problems.is_empty() {
            ""
        } else {
            " (!)"
        };
        format!(
            "{marker} {:<20} {status:<9} {when:<10} {}{warning}",
            truncate(&script.title, 20),
            script.bind.as_deref().unwrap_or(""),
        )
    }

    fn service_label(&self, index: usize) -> String {
        let Loaded::Ready(rows) = &self.services else {
            return String::new();
        };
        let row = &rows[index];
        let busy = self
            .busy
            .iter()
            .find(|(service, _)| service == &row.service)
            .map(|(_, verb)| verb);
        let (marker, active, sub) = match &row.state {
            None => ("   ", "unknown".to_string(), String::new()),
            Some(state) if !state.exists() => ("   ", "not found".to_string(), String::new()),
            Some(state) => {
                let marker = if state.is_enabled() {
                    "[*]"
                } else if state.can_toggle_enabled() {
                    "[ ]"
                } else {
                    " - "
                };
                (marker, state.active.clone(), state.sub.clone())
            }
        };
        let active = match busy {
            Some(verb) => format!("{}…", in_progress(*verb)),
            None => active,
        };
        let scope = match row.service.scope {
            Scope::System => "system",
            Scope::User => "user",
        };
        format!(
            "{marker} {:<26} {active:<11} {sub:<9} {scope}",
            truncate(&row.service.unit, 26)
        )
    }

    fn view_log(&self, frame: &mut Frame, log: &LogView) {
        let heading = match &log.source {
            LogSource::Script(_) => format!("output of {}", log.title),
            LogSource::Service(_) => format!("systemctl status {}", log.title),
        };
        let area = ui::draw_screen(frame, &heading);
        let dialog = ui::centred(
            area,
            area.width.saturating_sub(4).min(110),
            area.height.saturating_sub(2),
        );
        let inner = ui::draw_dialog(frame, dialog, &log.title);
        let parts = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        let summary = match (&log.source, log.loaded, &log.error) {
            (_, _, Some(error)) => error.clone(),
            (_, false, _) => "Loading…".to_string(),
            (LogSource::Script(_), true, _) if log.lines.is_empty() => {
                "No output from the last run.".to_string()
            }
            (LogSource::Script(_), true, _) if log.running => {
                format!(
                    "{} lines so far; still running.  <End> follows new output.",
                    log.lines.len()
                )
            }
            (LogSource::Script(_), true, _) => {
                format!(
                    "{} lines from the last run.  Red lines came from stderr.",
                    log.lines.len()
                )
            }
            (LogSource::Service(_), true, _) => {
                "Refreshed every two seconds.  Arrows and <PgUp>/<PgDn> scroll.".to_string()
            }
        };
        ui::draw_help(frame, parts[0], &[&summary]);

        let block = Block::new()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(ui::DIM_FG).bg(ui::DIALOG_BG))
            .style(ui::dialog_style());
        let text_area = block.inner(parts[1]);
        frame.render_widget(block, parts[1]);
        let height = text_area.height as usize;
        self.log_height.set(height);

        let bottom = log.lines.len().saturating_sub(height);
        let top = if log.follow {
            bottom
        } else {
            log.top.min(bottom)
        };
        let lines: Vec<Line> = log
            .lines
            .iter()
            .skip(top)
            .take(height)
            .map(|(text, tone)| {
                let style = match tone {
                    LineTone::Normal => ui::dialog_style(),
                    LineTone::Error => ui::dialog_style().fg(ui::HOTKEY_FG),
                    LineTone::Good => ui::dialog_style()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    LineTone::Dim => ui::dialog_style().fg(ui::DIM_FG),
                };
                Line::from(Span::styled(format!(" {text}"), style))
            })
            .collect();
        frame.render_widget(Paragraph::new(lines).style(ui::dialog_style()), text_area);
        draw_scroll_hint(frame, parts[1], top, height, log.lines.len());

        ui::draw_buttons(frame, parts[2], &[("Back", 'B')], 0);
    }

    fn view_add(&self, frame: &mut Frame, text: &str, error: Option<&str>) {
        let area = ui::draw_screen(frame, &self.heading());
        let dialog = ui::centred(area, 64, 12);
        let inner = ui::draw_dialog(frame, dialog, "Add a service");
        let parts = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(inner);
        ui::draw_help(
            frame,
            parts[0],
            &[
                "A systemd unit to show here, e.g. sshd or bluetooth.service.",
                "Add --user for one of your own user services.",
                "Adding only lists it; it does not start or enable it.",
            ],
        );
        frame.render_widget(
            Paragraph::new(Line::from(format!(" > {text}▏")))
                .style(Style::new().fg(ui::SELECTED_FG).bg(ui::SELECTED_BG)),
            parts[1],
        );
        if let Some(error) = error {
            frame.render_widget(
                Paragraph::new(Line::from(error.to_string())).style(
                    Style::new()
                        .fg(ui::HOTKEY_FG)
                        .bg(ui::DIALOG_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                parts[2],
            );
        }
        ui::draw_buttons(frame, parts[4], &[("Ok", '\0'), ("Cancel", '\0')], 0);
    }
}

/// Parses the add box: a unit name, optionally with `--user`.
fn parse_new_service(text: &str) -> Result<Service, String> {
    let mut scope = Scope::System;
    let mut unit = None;
    for word in text.split_whitespace() {
        match word {
            "--user" => scope = Scope::User,
            "--system" => scope = Scope::System,
            word if unit.is_none() => unit = Some(word),
            _ => return Err("one unit at a time, please".into()),
        }
    }
    Service::new(unit.unwrap_or(""), scope)
}

/// Colours `systemctl status` the way a terminal would: the state dot and
/// the Active line by state, the unit header bold.
fn colour_status(lines: Vec<String>, state: Option<&UnitState>) -> Vec<(String, LineTone)> {
    lines
        .into_iter()
        .map(|line| {
            let trimmed = line.trim_start();
            let tone = if trimmed.starts_with('●') || trimmed.starts_with('×') {
                match state {
                    Some(state) if state.is_failed() => LineTone::Error,
                    Some(state) if state.is_active() => LineTone::Good,
                    _ => LineTone::Normal,
                }
            } else if trimmed.starts_with('○') {
                LineTone::Dim
            } else if let Some(value) = trimmed.strip_prefix("Active:") {
                let value = value.trim_start();
                if value.starts_with("active") {
                    LineTone::Good
                } else if value.starts_with("failed") {
                    LineTone::Error
                } else {
                    LineTone::Dim
                }
            } else {
                LineTone::Normal
            };
            (line, tone)
        })
        .collect()
}

/// A small "12–40 of 80" marker on the log box's top-right border.
fn draw_scroll_hint(frame: &mut Frame, area: Rect, top: usize, height: usize, total: usize) {
    if total <= height || area.width < 20 {
        return;
    }
    let text = format!(" {}-{} of {total} ", top + 1, (top + height).min(total));
    let width = text.chars().count() as u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width + 2),
        y: area.y,
        width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(ui::DIM_FG).bg(ui::DIALOG_BG)),
        rect,
    );
}

fn in_progress(verb: Verb) -> &'static str {
    match verb {
        Verb::Start => "starting",
        Verb::Stop => "stopping",
        Verb::Restart => "restarting",
        Verb::Enable => "enabling",
        Verb::Disable => "disabling",
    }
}

/// Cuts text to `width` characters, marking the cut.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

/// A running time: `0:42`, `12:03`, `1:02:03`.
fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// How long ago, roughly: `12s ago`, `4m ago`, `2h ago`, `3d ago`.
fn ago(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s ago"),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use tessera_ipc::ExitStatus;

    fn ctx() -> Context {
        Context {
            under_tessera: true,
            terminal: vec!["foot".into()],
            share: 0.6,
            side: Side::Right,
            editor: vec!["nvim".into()],
            serv: "/opt/tessera/tessera-serv".into(),
            scripts_dir: "~/.config/tessera/scripts".into(),
        }
    }

    fn script(name: &str, state: ScriptState) -> ScriptInfo {
        ScriptInfo {
            name: name.into(),
            title: name.into(),
            description: None,
            path: format!("/home/me/.config/tessera/scripts/{name}"),
            mode: RunMode::Background,
            bind: None,
            autostart: false,
            problems: Vec::new(),
            state,
        }
    }

    fn unit(active: &str, enabled: &str) -> UnitState {
        UnitState {
            id: "x.service".into(),
            description: "An example".into(),
            load: "loaded".into(),
            active: active.into(),
            sub: if active == "active" {
                "running"
            } else {
                "dead"
            }
            .into(),
            enabled: enabled.into(),
        }
    }

    fn service(name: &str, scope: Scope, state: Option<UnitState>) -> ServiceRow {
        ServiceRow {
            service: Service::new(name, scope).unwrap(),
            state,
        }
    }

    /// A menu with two scripts and two services, as if both lists had arrived.
    fn loaded() -> ScriptsMenu {
        let mut menu = ScriptsMenu::new();
        let effects = menu.open(ctx());
        assert_eq!(effects, vec![Effect::ListScripts, Effect::QueryServices]);
        menu.set_scripts(Ok(vec![
            script("backup", ScriptState::NeverRun),
            script(
                "sleeper",
                ScriptState::Running {
                    pid: 7,
                    started_at_ms: tessera_ipc::unix_millis() - 42_000,
                },
            ),
        ]));
        menu.update(Update::Services(Ok(vec![
            service("sshd", Scope::System, Some(unit("active", "enabled"))),
            service("syncthing", Scope::User, Some(unit("inactive", "disabled"))),
        ])));
        menu
    }

    fn press(menu: &mut ScriptsMenu, code: KeyCode) -> Outcome {
        menu.key(Key::new(code))
    }

    fn effects(outcome: Outcome) -> Vec<Effect> {
        match outcome {
            Outcome::Stay(effects) => effects,
            Outcome::Close => panic!("expected to stay"),
        }
    }

    fn render(menu: &ScriptsMenu) -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
        terminal.draw(|frame| menu.view(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn both_lists_are_shown_with_their_state() {
        let screen = render(&loaded());
        assert!(screen.contains("Scripts & services"), "{screen}");
        assert!(screen.contains("backup"), "{screen}");
        assert!(screen.contains("never run"), "{screen}");
        assert!(screen.contains("running   0:42"), "{screen}");
        assert!(screen.contains("[*] sshd.service"), "{screen}");
        assert!(screen.contains("syncthing.service"), "{screen}");
        assert!(screen.contains("user"), "{screen}");
        assert!(screen.contains("2 scripts, 1 running"), "{screen}");
    }

    #[test]
    fn enter_runs_the_selected_script_and_t_opens_it_in_a_terminal() {
        let mut menu = loaded();
        assert_eq!(
            effects(press(&mut menu, KeyCode::Enter)),
            vec![Effect::RunScript {
                name: "backup".into(),
                mode: None
            }]
        );
        assert_eq!(
            effects(menu.key(Key::char('t'))),
            vec![Effect::RunScript {
                name: "backup".into(),
                mode: Some(RunMode::Terminal)
            }]
        );
    }

    #[test]
    fn editing_without_an_editor_says_so() {
        let mut menu = ScriptsMenu::new();
        menu.open(Context {
            editor: Vec::new(),
            ..ctx()
        });
        menu.set_scripts(Ok(vec![script("backup", ScriptState::NeverRun)]));
        assert_eq!(effects(menu.key(Key::char('e'))), Vec::new());
        assert!(render(&menu).contains("No editor found"));
    }

    #[test]
    fn the_script_buttons_include_kill() {
        let screen = render(&loaded());
        assert!(screen.contains("<Kill>"), "{screen}");
    }

    #[test]
    fn k_stops_and_a_toggles_autostart() {
        let mut menu = loaded();
        press(&mut menu, KeyCode::Down);
        assert_eq!(
            effects(menu.key(Key::char('k'))),
            vec![Effect::StopScript {
                name: "sleeper".into()
            }]
        );
        assert_eq!(
            effects(menu.key(Key::char('a'))),
            vec![Effect::SetAutostart {
                path: "/home/me/.config/tessera/scripts/sleeper".into(),
                on: true
            }]
        );
    }

    #[test]
    fn e_edits_in_a_terminal_beside_the_launcher() {
        let mut menu = loaded();
        assert_eq!(
            effects(menu.key(Key::char('e'))),
            vec![Effect::Spawn {
                argv: vec![
                    "foot".into(),
                    "-e".into(),
                    "nvim".into(),
                    "/home/me/.config/tessera/scripts/backup".into()
                ],
                placement: Placement::BesideCaller {
                    side: Side::Right,
                    ratio: 0.6
                },
            }]
        );
    }

    #[test]
    fn the_selection_crosses_the_headings_and_stays_on_services() {
        let mut menu = loaded();
        for _ in 0..10 {
            press(&mut menu, KeyCode::Down);
        }
        assert_eq!(
            menu.selected,
            Some(Selected::Service(
                Service::new("syncthing", Scope::User).unwrap()
            ))
        );
        press(&mut menu, KeyCode::Up);
        assert_eq!(
            menu.selected,
            Some(Selected::Service(
                Service::new("sshd", Scope::System).unwrap()
            ))
        );
    }

    #[test]
    fn enter_on_a_service_toggles_it() {
        let mut menu = loaded();
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down); // sshd, active
        let sshd = Service::new("sshd", Scope::System).unwrap();
        assert_eq!(
            effects(press(&mut menu, KeyCode::Enter)),
            vec![Effect::ServiceAction {
                service: sshd.clone(),
                verb: Verb::Stop
            }]
        );
        assert!(render(&menu).contains("stopping…"));
        assert_eq!(
            effects(press(&mut menu, KeyCode::Enter)),
            Vec::new(),
            "one action at a time per service"
        );

        press(&mut menu, KeyCode::Down); // syncthing, inactive
        assert_eq!(
            effects(menu.key(Key::char(' '))),
            vec![Effect::ServiceAction {
                service: Service::new("syncthing", Scope::User).unwrap(),
                verb: Verb::Start
            }]
        );
    }

    #[test]
    fn a_password_request_moves_to_a_tiled_terminal() {
        let mut menu = loaded();
        let sshd = Service::new("sshd", Scope::System).unwrap();
        let effects = menu.update(Update::ServiceDone {
            service: sshd,
            verb: Verb::Stop,
            result: Err(ActionError::NeedsAuth),
        });
        assert_eq!(
            effects[0],
            Effect::Spawn {
                argv: vec![
                    "foot".into(),
                    "-e".into(),
                    "/opt/tessera/tessera-serv".into(),
                    "stop".into(),
                    "--pause".into(),
                    "sshd.service".into()
                ],
                placement: Placement::BesideCaller {
                    side: Side::Right,
                    ratio: 0.6
                },
            }
        );
        assert_eq!(effects[1], Effect::QueryServices, "and the list refreshes");
        assert!(render(&menu).contains("needs a password"));
    }

    #[test]
    fn user_services_keep_their_scope_in_the_terminal_command() {
        let menu = loaded();
        let argv = menu.serv_argv(
            Verb::Start,
            &Service::new("syncthing", Scope::User).unwrap(),
        );
        assert_eq!(
            argv,
            [
                "/opt/tessera/tessera-serv",
                "start",
                "--user",
                "--pause",
                "syncthing.service"
            ]
        );
    }

    #[test]
    fn a_failure_is_shown_in_red_and_does_not_open_a_terminal() {
        let mut menu = loaded();
        let effects = menu.update(Update::ServiceDone {
            service: Service::new("sshd", Scope::System).unwrap(),
            verb: Verb::Start,
            result: Err(ActionError::Failed("Unit sshd.service is masked.".into())),
        });
        assert_eq!(effects, vec![Effect::QueryServices]);
        assert!(render(&menu).contains("masked"));
    }

    #[test]
    fn static_units_explain_why_they_cannot_be_enabled() {
        let mut menu = loaded();
        menu.update(Update::Services(Ok(vec![service(
            "systemd-journald",
            Scope::System,
            Some(unit("active", "static")),
        )])));
        for _ in 0..5 {
            press(&mut menu, KeyCode::Down);
        }
        assert_eq!(effects(menu.key(Key::char('a'))), Vec::new());
        assert!(render(&menu).contains("cannot be enabled"));
    }

    #[test]
    fn s_opens_a_live_status_box_for_the_service() {
        let mut menu = loaded();
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down);
        let sshd = Service::new("sshd", Scope::System).unwrap();
        assert_eq!(
            effects(menu.key(Key::char('s'))),
            vec![Effect::ServiceStatus {
                service: sshd.clone()
            }]
        );
        menu.update(Update::ServiceStatus {
            service: sshd.clone(),
            result: Ok(vec![
                "● sshd.service - OpenSSH Daemon".into(),
                "     Active: active (running) since today".into(),
            ]),
        });
        let screen = render(&menu);
        assert!(screen.contains("OpenSSH Daemon"), "{screen}");
        assert!(screen.contains("Active: active"), "{screen}");

        // Ticks keep it fresh.
        menu.update(Update::Tick);
        let effects = menu.update(Update::Tick);
        assert!(effects.contains(&Effect::ServiceStatus { service: sshd }));
        press(&mut menu, KeyCode::Esc);
        assert!(render(&menu).contains("Services"), "back to the list");
    }

    #[test]
    fn status_lines_are_coloured_by_state() {
        let lines = colour_status(
            vec![
                "● x.service - X".into(),
                "     Active: failed (Result: exit-code)".into(),
                "Sep 13 journal".into(),
            ],
            Some(&unit("failed", "enabled")),
        );
        assert_eq!(lines[0].1, LineTone::Error);
        assert_eq!(lines[1].1, LineTone::Error);
        assert_eq!(lines[2].1, LineTone::Normal);
    }

    #[test]
    fn the_output_view_follows_a_running_script() {
        let mut menu = loaded();
        press(&mut menu, KeyCode::Down); // sleeper
        assert_eq!(
            effects(menu.key(Key::char('o'))),
            vec![Effect::ScriptOutput {
                name: "sleeper".into()
            }]
        );
        let lines: Vec<OutputLine> = (0..100)
            .map(|index| OutputLine {
                text: format!("line {index}"),
                stderr: index == 99,
            })
            .collect();
        menu.set_output("sleeper", Ok((lines, true)));
        let screen = render(&menu);
        assert!(screen.contains("line 99"), "follows the end: {screen}");
        assert!(!screen.contains("line 0 "), "{screen}");
        assert!(
            menu.update(Update::Tick).contains(&Effect::ScriptOutput {
                name: "sleeper".into()
            }),
            "polls while running"
        );

        press(&mut menu, KeyCode::Home);
        let screen = render(&menu);
        assert!(screen.contains("line 0"), "{screen}");
        assert!(!screen.contains("line 99"), "{screen}");
    }

    #[test]
    fn an_exit_event_refreshes_the_list_and_the_open_output() {
        let mut menu = loaded();
        press(&mut menu, KeyCode::Down);
        menu.key(Key::char('o'));
        let effects = menu.update(Update::Compositor(IpcEvent::ScriptExited {
            name: "sleeper".into(),
            exit: ExitStatus::Signal { signal: 15 },
            duration_ms: 42_000,
        }));
        assert_eq!(
            effects,
            vec![
                Effect::ListScripts,
                Effect::ScriptOutput {
                    name: "sleeper".into()
                }
            ]
        );
    }

    #[test]
    fn new_services_are_typed_in_with_an_optional_user_flag() {
        let mut menu = loaded();
        menu.key(Key::char('n'));
        for ch in "--user syncthing".chars() {
            menu.key(Key::char(ch));
        }
        assert_eq!(
            effects(press(&mut menu, KeyCode::Enter)),
            vec![Effect::AddService {
                service: Service::new("syncthing", Scope::User).unwrap()
            }]
        );

        menu.key(Key::char('n'));
        for ch in "two units".chars() {
            menu.key(Key::char(ch));
        }
        assert_eq!(effects(press(&mut menu, KeyCode::Enter)), Vec::new());
        assert!(render(&menu).contains("one unit at a time"));
        press(&mut menu, KeyCode::Esc);
        assert!(render(&menu).contains("Services"));
    }

    #[test]
    fn outside_tessera_only_services_work() {
        let mut menu = ScriptsMenu::new();
        let outside = Context {
            under_tessera: false,
            ..ctx()
        };
        assert_eq!(menu.open(outside), vec![Effect::QueryServices]);
        assert!(render(&menu).contains("not running under Tessera"));
    }

    #[test]
    fn script_problems_show_when_selected() {
        let mut menu = loaded();
        let mut broken = script("broken", ScriptState::NeverRun);
        broken.problems = vec!["bind: Mod+Shift+E already does \"quit Tessera\"".into()];
        menu.set_scripts(Ok(vec![broken]));
        let screen = render(&menu);
        assert!(screen.contains("(!)"), "{screen}");
        assert!(screen.contains("quit Tessera"), "{screen}");
    }

    #[test]
    fn service_polls_do_not_pile_up() {
        let mut menu = loaded();
        assert_eq!(menu.update(Update::Tick), Vec::new());
        assert_eq!(menu.update(Update::Tick), vec![Effect::QueryServices]);
        menu.update(Update::Tick);
        assert_eq!(
            menu.update(Update::Tick),
            Vec::new(),
            "the first query has not answered yet"
        );
    }

    #[test]
    fn b_and_esc_leave_the_screen() {
        let mut menu = loaded();
        assert_eq!(menu.key(Key::char('b')), Outcome::Close);
        let mut menu = loaded();
        assert_eq!(press(&mut menu, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn times_read_naturally() {
        assert_eq!(clock(42_000), "0:42");
        assert_eq!(clock(3_723_000), "1:02:03");
        assert_eq!(ago(5_000), "5s ago");
        assert_eq!(ago(240_000), "4m ago");
        assert_eq!(ago(7_200_000), "2h ago");
        assert_eq!(truncate("a-very-long-script-name", 10), "a-very-lo…");
    }
}
