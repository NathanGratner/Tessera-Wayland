//! Screens, navigation and what each menu entry does.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::ListItem,
};

use tessera_config::{ConfigValues, split_command};
use tessera_ipc::{Placement, RunMode, ScriptInfo, Side, WindowId, WindowInfo};
use tessera_services::{PowerAction, Service, Verb};

use crate::{
    apps::Catalog,
    config_menu::{ConfigMenu, Outcome, SaveReport},
    event::{Event, Key, KeyCode, MouseKind},
    power_menu::{self, PowerMenu},
    scripts_menu::{self, ScriptsMenu},
    ui::{self, Tone},
    worker::Update,
};

/// Something the front end should do on the app's behalf.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Start a program. Sent to the compositor when running under Tessera, so
    /// that it can place the window; run directly otherwise.
    Spawn {
        /// Program and arguments, already split; never passed through a shell.
        argv: Vec<String>,
        /// Where the window should go.
        placement: Placement,
    },
    /// Ask the compositor for the current window list.
    ListWindows,
    /// Give a window keyboard focus, switching workspace if needed.
    Focus(WindowId),
    /// Write this configuration to config.toml, then ask the compositor to reload.
    SaveConfig {
        /// The whole file, as it should be written.
        toml: String,
    },
    /// Re-read config.toml, discarding unsaved edits.
    LoadConfig,
    /// Ask the compositor for its scripts.
    ListScripts,
    /// Ask the compositor to run a script.
    RunScript {
        /// The script's file name.
        name: String,
        /// How; the script's header decides when `None`.
        mode: Option<RunMode>,
    },
    /// Ask the compositor to stop a script.
    StopScript {
        /// The script's file name.
        name: String,
    },
    /// Fetch a script's latest output.
    ScriptOutput {
        /// The script's file name.
        name: String,
    },
    /// Rewrite a script's `autostart` header line.
    SetAutostart {
        /// The script file.
        path: String,
        /// The new value.
        on: bool,
    },
    /// Ask systemd about the tracked services, off the event loop.
    QueryServices,
    /// Run `systemctl <verb>` on a service, off the event loop.
    ServiceAction {
        /// Which service.
        service: Service,
        /// What to do.
        verb: Verb,
    },
    /// Fetch `systemctl status` for a service, off the event loop.
    ServiceStatus {
        /// Which service.
        service: Service,
    },
    /// Add a service to `services.toml`.
    AddService {
        /// The service to show.
        service: Service,
    },
    /// Remove a service from `services.toml`, leaving the service alone.
    RemoveService {
        /// The service to stop showing.
        service: Service,
    },
    /// Ask the compositor to end the session.
    EndSession,
    /// Suspend, reboot or power off, through logind, off the event loop.
    Power(PowerAction),
    /// Leave the launcher.
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    OpenApps,
    OpenWindows,
    OpenConfig,
    OpenScripts,
    OpenPower,
    SpawnTerminal,
}

struct MenuItem {
    label: &'static str,
    hotkey: char,
    action: Action,
}

const MAIN_MENU: &[MenuItem] = &[
    MenuItem {
        label: "Launch application  --->",
        hotkey: 'L',
        action: Action::OpenApps,
    },
    MenuItem {
        label: "Terminal, tiled beside launcher",
        hotkey: 'T',
        action: Action::SpawnTerminal,
    },
    MenuItem {
        label: "Scripts & services  --->",
        hotkey: 'S',
        action: Action::OpenScripts,
    },
    MenuItem {
        label: "Configuration  --->",
        hotkey: 'C',
        action: Action::OpenConfig,
    },
    MenuItem {
        label: "Windows  --->",
        hotkey: 'W',
        action: Action::OpenWindows,
    },
    MenuItem {
        label: "Power & session  --->",
        hotkey: 'P',
        action: Action::OpenPower,
    },
];

const HELP_TEXT: &str = "\
Arrow keys or j/k move the selection.
<Enter> opens the selected entry.
A highlighted letter jumps straight to an entry,
including the letters in the buttons below.
<Esc>, <q> or <b> go back one screen.
</> filters the application list.
<?> shows this help.";

enum Screen {
    Main {
        selected: usize,
    },
    Apps {
        selected: usize,
        filter: String,
        filtering: bool,
    },
    Message {
        title: &'static str,
        body: String,
    },
    Windows {
        selected: usize,
        /// Filled in when the compositor answers; empty until then.
        windows: Vec<WindowInfo>,
        loaded: bool,
    },
    /// The configuration menu, whose own state lives in [`App::config`].
    Config,
    /// Scripts & services, whose own state lives in [`App::scripts`].
    Scripts,
    /// Power & session, whose own state lives in [`App::power`].
    Power,
}

pub struct App {
    stack: Vec<Screen>,
    catalog: Catalog,
    /// Kept between visits, so leaving the menu does not lose unsaved edits.
    config: ConfigMenu,
    /// Kept between visits, so returning shows the last lists at once.
    scripts: ScriptsMenu,
    power: PowerMenu,
    status: Option<String>,
    quit: bool,
    /// False when there is no compositor socket, so placement requests are
    /// impossible and programs are started directly.
    under_tessera: bool,
    /// The application overlay (`--apps`): only the application list, and
    /// leaving it or launching from it quits.
    apps_only: bool,
}

impl App {
    pub fn new(catalog: Catalog, config: ConfigValues) -> Self {
        Self {
            stack: vec![Screen::Main { selected: 0 }],
            catalog,
            config: ConfigMenu::new(config),
            scripts: ScriptsMenu::new(),
            power: PowerMenu::default(),
            status: None,
            quit: false,
            under_tessera: tessera_ipc::socket_from_env().is_some(),
            apps_only: false,
        }
    }

    /// The application overlay: starts on the application list with the
    /// filter already on, so typing narrows it straight away. Enter launches
    /// and quits; Esc quits.
    pub fn apps_only(catalog: Catalog, config: ConfigValues) -> Self {
        let mut app = Self::new(catalog, config);
        app.stack = vec![Screen::Apps {
            selected: 0,
            filter: String::new(),
            filtering: true,
        }];
        app.apps_only = true;
        app
    }

    /// Overrides the "running under Tessera" detection, for tests.
    #[cfg(test)]
    pub fn with_tessera(mut self, under_tessera: bool) -> Self {
        self.under_tessera = under_tessera;
        self
    }

    /// Terminals open on the side of the launcher away from its configured
    /// side (`launcher.side`), so the launcher keeps to its edge.
    fn terminal_side(&self) -> Side {
        Side::from_name(&self.config.saved().text("launcher.side"))
            .unwrap_or(Side::Left)
            .opposite()
    }

    /// What the Scripts & services screen needs, from the saved settings and
    /// the environment.
    fn scripts_context(&self) -> scripts_menu::Context {
        let editor = find_editor(
            &self.config.saved().text("general.editor"),
            |name| std::env::var(name).ok(),
            on_path,
        );
        scripts_menu::Context {
            under_tessera: self.under_tessera,
            terminal: self.terminal(),
            share: self.config.saved().int("launcher.beside_share") as f32 / 100.0,
            side: self.terminal_side(),
            editor,
            serv: tessera_serv(),
            scripts_dir: home_relative(&tessera_config::scripts::scripts_dir()),
        }
    }

    /// The compositor's script list arrived.
    pub fn scripts_listed(&mut self, result: Result<Vec<ScriptInfo>, String>) {
        self.scripts.set_scripts(result);
    }

    /// A script's output arrived.
    pub fn script_output(&mut self, name: &str, result: scripts_menu::OutputResult) {
        self.scripts.set_output(name, result);
    }

    /// A script request finished.
    pub fn script_report(&mut self, result: Result<String, String>) {
        self.scripts.report(result);
    }

    /// Asking the compositor to end the session failed.
    pub fn session_report(&mut self, result: Result<(), String>) {
        self.power.report(result);
    }

    /// What the Power & session screen needs to know.
    fn power_context(&self) -> power_menu::Context {
        power_menu::Context {
            under_tessera: self.under_tessera,
            nested: std::env::var("TESSERA_BACKEND").is_ok_and(|backend| backend == "nested"),
            terminal: self.terminal(),
            share: self.config.saved().int("launcher.beside_share") as f32 / 100.0,
            side: self.terminal_side(),
        }
    }

    /// Background work finished, the compositor sent an event, or a second passed.
    ///
    /// Only the Scripts & services screen listens. While it is hidden, ticks
    /// and compositor events are ignored, so nothing polls in the background;
    /// results of work already started still land, so a password fallback
    /// still opens its terminal.
    pub fn background(&mut self, update: Update) -> Vec<Effect> {
        // A power action's answer matters even if the screen has been left:
        // it may be a password request that needs its terminal.
        if matches!(update, Update::PowerDone { .. }) {
            return self.power.update(update);
        }
        let visible = matches!(self.stack.last(), Some(Screen::Scripts));
        if !visible && matches!(update, Update::Tick | Update::Compositor(_)) {
            return Vec::new();
        }
        self.scripts.update(update)
    }

    /// Whether the screen shows something that changes by itself (timers),
    /// so the front end should redraw on every tick.
    pub fn is_live(&self) -> bool {
        matches!(self.stack.last(), Some(Screen::Scripts))
    }

    /// What the status line along the top says.
    fn heading(&self) -> &'static str {
        if self.under_tessera {
            "tessera-launcher"
        } else {
            "tessera-launcher · not running under Tessera: windows will not be tiled"
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Shows a problem with config.toml the next time the menu opens.
    pub fn config_problem(&mut self, text: impl Into<String>) {
        self.config.set_message(text, Tone::Error);
    }

    /// Records how a save went.
    pub fn config_saved(&mut self, result: Result<SaveReport, String>) {
        self.config.save_finished(result);
    }

    /// Records how a reload of the file went.
    pub fn config_loaded(&mut self, result: Result<ConfigValues, String>) {
        self.config.load_finished(result);
    }

    /// The terminal command, as last saved.
    fn terminal(&self) -> Vec<String> {
        let argv = split_command(&self.config.saved().text("general.terminal"));
        if argv.is_empty() {
            vec!["foot".into()]
        } else {
            argv
        }
    }

    /// Handles one input event, returning whatever the front end must carry out.
    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        let Event::Key(key) = event else {
            if let Event::Mouse { kind, .. } = event {
                match kind {
                    MouseKind::ScrollUp => self.move_selection(-1),
                    MouseKind::ScrollDown => self.move_selection(1),
                    MouseKind::Press => {}
                }
            }
            if event == Event::Closed {
                self.quit = true;
                return vec![Effect::Quit];
            }
            return Vec::new();
        };

        // Auto-repeat is for scrolling and deleting, not for activating things.
        if !key.acts_on_repeat() {
            return Vec::new();
        }

        self.status = None;
        match self.stack.last_mut() {
            Some(Screen::Power) => match self.power.key(key) {
                power_menu::Outcome::Stay(effects) => effects,
                power_menu::Outcome::Close => {
                    self.pop();
                    Vec::new()
                }
            },
            Some(Screen::Scripts) => match self.scripts.key(key) {
                scripts_menu::Outcome::Stay(effects) => effects,
                scripts_menu::Outcome::Close => {
                    self.pop();
                    Vec::new()
                }
            },
            Some(Screen::Config) => match self.config.key(key) {
                Outcome::Stay => Vec::new(),
                Outcome::Close => {
                    self.pop();
                    Vec::new()
                }
                Outcome::Effect(effect) => vec![effect],
            },
            Some(Screen::Apps {
                filtering: true, ..
            }) => self.key_in_filter(key),
            Some(Screen::Message { .. }) => {
                // Only keys that mean "dismiss" close a note; anything else is
                // ignored rather than swallowed and acted on twice.
                if matches!(
                    key.code,
                    KeyCode::Enter
                        | KeyCode::Esc
                        | KeyCode::Char(' ')
                        | KeyCode::Char('q')
                        | KeyCode::Char('b')
                ) {
                    self.pop();
                }
                Vec::new()
            }
            _ => self.key_in_menu(key),
        }
    }

    fn key_in_menu(&mut self, key: Key) -> Vec<Effect> {
        match key.code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Esc => self.pop(),
            KeyCode::Enter => return self.activate(),
            KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('q') => {
                if self.stack.len() == 1 {
                    self.quit = true;
                    return vec![Effect::Quit];
                }
                self.pop();
            }
            KeyCode::Char('?') => self.push(Screen::Message {
                title: "Help",
                body: HELP_TEXT.to_string(),
            }),
            KeyCode::Char('/') => {
                if let Some(Screen::Apps { filtering, .. }) = self.stack.last_mut() {
                    *filtering = true;
                }
            }
            KeyCode::Char(ch) => return self.hotkey(ch),
            _ => {}
        }
        Vec::new()
    }

    fn key_in_filter(&mut self, key: Key) -> Vec<Effect> {
        match key.code {
            // The overlay is always filtering, so Esc is the way out of it.
            KeyCode::Esc if self.apps_only => {
                self.quit = true;
                return vec![Effect::Quit];
            }
            KeyCode::Esc => {
                if let Some(Screen::Apps {
                    filtering, filter, ..
                }) = self.stack.last_mut()
                {
                    *filtering = false;
                    filter.clear();
                }
            }
            KeyCode::Enter => {
                if let Some(Screen::Apps { filtering, .. }) = self.stack.last_mut() {
                    *filtering = false;
                }
                return self.activate();
            }
            KeyCode::Backspace => {
                if let Some(Screen::Apps { filter, .. }) = self.stack.last_mut() {
                    filter.pop();
                }
            }
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            _ => {
                if let (
                    Some(ch),
                    Some(Screen::Apps {
                        filter, selected, ..
                    }),
                ) = (key.typed(), self.stack.last_mut())
                {
                    filter.push(ch);
                    *selected = 0;
                }
            }
        }
        Vec::new()
    }

    fn hotkey(&mut self, ch: char) -> Vec<Effect> {
        // The button row along the bottom highlights a letter in each button;
        // those letters work, except where the screen uses typing for filtering.
        match self.stack.last() {
            Some(Screen::Main { .. }) if ch.eq_ignore_ascii_case(&'x') => {
                self.quit = true;
                return vec![Effect::Quit];
            }
            Some(Screen::Main { .. }) if ch.eq_ignore_ascii_case(&'h') => {
                self.push(Screen::Message {
                    title: "Help",
                    body: HELP_TEXT.to_string(),
                });
                return Vec::new();
            }
            Some(Screen::Windows { .. }) if ch.eq_ignore_ascii_case(&'b') => {
                self.pop();
                return Vec::new();
            }
            _ => {}
        }

        match self.stack.last() {
            Some(Screen::Main { .. }) => {
                let found = MAIN_MENU
                    .iter()
                    .position(|item| item.hotkey.eq_ignore_ascii_case(&ch));
                if let Some(index) = found {
                    if let Some(Screen::Main { selected }) = self.stack.last_mut() {
                        *selected = index;
                    }
                    return self.activate();
                }
            }
            Some(Screen::Apps { .. }) => {
                // On the app list, typing starts filtering straight away.
                if let Some(Screen::Apps {
                    filtering,
                    filter,
                    selected,
                }) = self.stack.last_mut()
                {
                    *filtering = true;
                    filter.push(ch);
                    *selected = 0;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn activate(&mut self) -> Vec<Effect> {
        match self.stack.last() {
            Some(Screen::Main { selected }) => {
                let action = MAIN_MENU[(*selected).min(MAIN_MENU.len() - 1)].action;
                match action {
                    Action::OpenConfig => {
                        self.config.open();
                        self.push(Screen::Config);
                    }
                    Action::OpenScripts => {
                        let ctx = self.scripts_context();
                        self.push(Screen::Scripts);
                        return self.scripts.open(ctx);
                    }
                    Action::OpenPower => {
                        let ctx = self.power_context();
                        self.power.open(ctx);
                        self.push(Screen::Power);
                    }
                    Action::OpenWindows => {
                        self.push(Screen::Windows {
                            selected: 0,
                            windows: Vec::new(),
                            loaded: false,
                        });
                        return vec![Effect::ListWindows];
                    }
                    Action::OpenApps => {
                        if self.catalog.is_empty() {
                            self.status = Some("no applications found".into());
                        } else {
                            self.push(Screen::Apps {
                                selected: 0,
                                filter: String::new(),
                                filtering: false,
                            });
                        }
                    }
                    Action::SpawnTerminal => {
                        // The whole point of the IPC: the terminal lands beside
                        // this window, taking its configured share (design §8).
                        let share = self.config.saved().int("launcher.beside_share");
                        return vec![Effect::Spawn {
                            argv: self.terminal(),
                            placement: Placement::BesideCaller {
                                side: self.terminal_side(),
                                ratio: share as f32 / 100.0,
                            },
                        }];
                    }
                }
            }
            Some(Screen::Apps {
                selected, filter, ..
            }) => {
                let matches = self.catalog.search(filter);
                if let Some(app) = matches.get(*selected) {
                    let mut argv = split_command(&app.command);
                    if app.terminal {
                        let mut wrapper = self.terminal();
                        wrapper.push("-e".into());
                        wrapper.append(&mut argv);
                        argv = wrapper;
                    }
                    let spawn = Effect::Spawn {
                        argv,
                        placement: Placement::Auto,
                    };
                    // The overlay has done its job once something is launched.
                    if self.apps_only {
                        self.quit = true;
                        return vec![spawn, Effect::Quit];
                    }
                    return vec![spawn];
                }
                self.status = Some("nothing matches that filter".into());
            }
            Some(Screen::Windows {
                selected, windows, ..
            }) => {
                if let Some(window) = windows.get(*selected) {
                    let id = window.id;
                    self.pop();
                    return vec![Effect::Focus(id), Effect::Quit];
                }
                self.status = Some("no windows to switch to".into());
            }
            _ => {}
        }
        Vec::new()
    }

    /// Records the window list the compositor sent back.
    pub fn set_windows(&mut self, list: Vec<WindowInfo>) {
        if let Some(Screen::Windows {
            windows,
            loaded,
            selected,
        }) = self.stack.last_mut()
        {
            *selected = list
                .iter()
                .position(|window| window.focused)
                .unwrap_or(0)
                .min(list.len().saturating_sub(1));
            *windows = list;
            *loaded = true;
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let count = self.visible_count();
        if count == 0 {
            return;
        }
        let selected = match self.stack.last_mut() {
            Some(Screen::Main { selected })
            | Some(Screen::Apps { selected, .. })
            | Some(Screen::Windows { selected, .. }) => selected,
            _ => return,
        };
        let next = (*selected as i32 + delta).clamp(0, count as i32 - 1);
        *selected = next as usize;
    }

    fn visible_count(&self) -> usize {
        match self.stack.last() {
            Some(Screen::Main { .. }) => MAIN_MENU.len(),
            Some(Screen::Apps { filter, .. }) => self.catalog.search(filter).len(),
            Some(Screen::Windows { windows, .. }) => windows.len(),
            _ => 0,
        }
    }

    fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
    }

    /// Goes back one screen. In the overlay there is nothing to go back to,
    /// so leaving the application list quits.
    fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        } else if self.apps_only {
            self.quit = true;
        }
    }

    pub fn view(&self, frame: &mut Frame) {
        match self.stack.last() {
            Some(Screen::Main { selected }) => self.view_main(frame, *selected),
            Some(Screen::Apps {
                selected,
                filter,
                filtering,
            }) => self.view_apps(frame, *selected, filter, *filtering),
            Some(Screen::Message { title, body }) => self.view_message(frame, title, body),
            Some(Screen::Windows {
                selected,
                windows,
                loaded,
            }) => self.view_windows(frame, *selected, windows, *loaded),
            Some(Screen::Config) => self.config.view(frame),
            Some(Screen::Scripts) => self.scripts.view(frame),
            Some(Screen::Power) => self.power.view(frame, self.heading()),
            None => {}
        }
    }

    fn view_main(&self, frame: &mut Frame, selected: usize) {
        let area = ui::draw_screen(frame, self.heading());
        let dialog = ui::centred(area, 54, (MAIN_MENU.len() as u16) + 11);
        let inner = ui::draw_dialog(frame, dialog, "Tessera");

        let rows = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        ui::draw_help(
            frame,
            rows[0],
            &[
                "Arrow keys navigate the menu.  <Enter> selects",
                "submenus --->.  Highlighted letters are hotkeys.",
                "<Esc><Esc> goes back, <?> shows help.",
            ],
        );

        let items: Vec<ListItem> = MAIN_MENU
            .iter()
            .enumerate()
            .map(|(index, item)| {
                ListItem::new(ui::menu_line(
                    item.label,
                    Some(item.hotkey),
                    index == selected,
                ))
            })
            .collect();
        self.draw_list(frame, rows[1], items, selected);

        ui::draw_buttons(
            frame,
            rows[2],
            &[("Select", 'S'), ("Exit", 'x'), ("Help", 'H')],
            0,
        );
    }

    fn view_apps(&self, frame: &mut Frame, selected: usize, filter: &str, filtering: bool) {
        let matches = self.catalog.search(filter);
        // The overlay's surface is the dialog, sized by the front end, so
        // there is no screen around it to paint.
        let dialog = if self.apps_only {
            frame.area()
        } else {
            let area = ui::draw_screen(frame, "tessera-launcher · applications");
            ui::centred(area, 60, area.height.saturating_sub(4).min(24))
        };
        let inner = ui::draw_dialog(frame, dialog, "Launch application");

        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        let prompt = if filtering || !filter.is_empty() {
            format!("Filter: {filter}▏")
        } else {
            format!(
                "{} applications.  Type or press </> to filter.",
                matches.len()
            )
        };
        ui::draw_help(frame, rows[0], &[&prompt]);

        let items: Vec<ListItem> = matches
            .iter()
            .enumerate()
            .map(|(index, app)| ListItem::new(ui::menu_line(&app.name, None, index == selected)))
            .collect();
        self.draw_list(frame, rows[1], items, selected);

        if self.apps_only {
            let hint = match &self.status {
                Some(status) => status.clone(),
                None => "<Enter> launches.  <Esc> closes.".to_string(),
            };
            ui::draw_help(frame, rows[2], &[&hint]);
        } else {
            ui::draw_buttons(frame, rows[2], &[("Launch", 'L'), ("Back", 'B')], 0);
        }
    }

    fn view_windows(
        &self,
        frame: &mut Frame,
        selected: usize,
        windows: &[WindowInfo],
        loaded: bool,
    ) {
        let area = ui::draw_screen(frame, self.heading());
        let dialog = ui::centred(area, 64, area.height.saturating_sub(4).min(20));
        let inner = ui::draw_dialog(frame, dialog, "Windows");

        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        let summary = match (loaded, windows.len()) {
            (false, _) => "Asking the compositor…".to_string(),
            (true, 0) => "No windows open.".to_string(),
            (true, 1) => "1 window.  <Enter> switches to it.".to_string(),
            (true, count) => format!("{count} windows.  <Enter> switches to the selected one."),
        };
        ui::draw_help(frame, rows[0], &[&summary]);

        let items: Vec<ListItem> = windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                // "1  foot            ~/Projects/tessera" — workspace, app, title.
                let name = window.app_id.as_deref().unwrap_or("(unknown)");
                let title = window.title.as_deref().unwrap_or("");
                let marker = if window.focused { '*' } else { ' ' };
                let label = format!("{}{}  {:<16} {}", marker, window.workspace, name, title);
                ListItem::new(ui::menu_line(&label, None, index == selected))
            })
            .collect();
        self.draw_list(frame, rows[1], items, selected);

        ui::draw_buttons(frame, rows[2], &[("Switch", 'S'), ("Back", 'B')], 0);
    }

    fn view_message(&self, frame: &mut Frame, title: &str, body: &str) {
        ui::draw_note(frame, self.heading(), title, body);
    }

    fn draw_list(&self, frame: &mut Frame, area: Rect, items: Vec<ListItem<'_>>, selected: usize) {
        let status = self.status.as_deref().map(|text| (text, Tone::Info));
        ui::draw_list(frame, area, items, selected, status);
    }
}

/// How to run `tessera-serv`: beside this binary when built together, as in
/// `target/debug`, otherwise from `PATH`.
fn tessera_serv() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tessera-serv")))
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "tessera-serv".into())
}

/// Editors tried, in order, when neither `$VISUAL` nor `$EDITOR` names one.
/// Editors tried, in order, when the configured one is not installed and
/// neither `$VISUAL` nor `$EDITOR` names one that is.
const FALLBACK_EDITORS: [&str; 7] = ["nano", "nvim", "vim", "vi", "hx", "micro", "emacs"];

/// The editor to open scripts in: the `general.editor` setting (nano unless
/// changed), then `$VISUAL`, `$EDITOR`, then the first of
/// [`FALLBACK_EDITORS`] that is installed. Empty when there is none.
///
/// Only an editor that exists is chosen. A terminal told to run a missing
/// program opens and closes in the same instant, which looks like nothing
/// happened at all; that was the first version of this.
fn find_editor(
    configured: &str,
    var: impl Fn(&str) -> Option<String>,
    exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    std::iter::once(split_command(configured))
        .chain(
            ["VISUAL", "EDITOR"]
                .iter()
                .filter_map(|name| var(name))
                .map(|value| split_command(&value)),
        )
        .chain(FALLBACK_EDITORS.iter().map(|name| vec![name.to_string()]))
        .find(|argv| argv.first().is_some_and(|program| exists(program)))
        .unwrap_or_default()
}

/// Whether a program can be run: a path to a file, or a name found on `PATH`.
fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return std::path::Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// `/home/me/x` as `~/x`, for headings.
fn home_relative(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(std::path::PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Key;
    use ratatui::{Terminal, backend::TestBackend};

    fn app() -> App {
        App::new(Catalog::default(), ConfigValues::default())
    }

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
        terminal.draw(|frame| app.view(frame)).unwrap();
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

    fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.update(Event::Key(Key::new(code)))
    }

    #[test]
    fn the_main_menu_shows_its_entries_and_title() {
        let screen = render(&app());
        assert!(screen.contains("Tessera"), "{screen}");
        assert!(screen.contains("Launch application  --->"), "{screen}");
        assert!(
            screen.contains("Terminal, tiled beside launcher"),
            "{screen}"
        );
        assert!(screen.contains("Select"), "{screen}");
    }

    #[test]
    fn arrows_move_the_selection_and_stop_at_the_ends() {
        let mut app = app();
        press(&mut app, KeyCode::Up);
        assert_eq!(app.visible_count(), MAIN_MENU.len());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        match app.stack.last() {
            Some(Screen::Main { selected }) => assert_eq!(*selected, 2),
            _ => panic!("still expected the main menu"),
        }
        for _ in 0..20 {
            press(&mut app, KeyCode::Down);
        }
        match app.stack.last() {
            Some(Screen::Main { selected }) => assert_eq!(*selected, MAIN_MENU.len() - 1),
            _ => panic!("still expected the main menu"),
        }
    }

    #[test]
    fn the_terminal_entry_spawns_the_configured_command() {
        let mut app = app();
        let effects = app.update(Event::Key(Key::char('t')));
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                argv: vec!["foot".into()],
                placement: Placement::BesideCaller {
                    side: Side::Right,
                    ratio: 0.6,
                },
            }],
            "the terminal entry asks for a tile beside the launcher"
        );
    }

    #[test]
    fn every_main_menu_entry_opens_something_real() {
        // Power & session was the last entry that only said "arrives in S7".
        let mut app = app().with_tessera(true);
        app.update(Event::Key(Key::char('p')));
        let screen = render(&app);
        assert!(screen.contains("Power off"), "{screen}");
        assert!(!screen.contains("Not built yet"), "{screen}");
        press(&mut app, KeyCode::Esc);
        assert!(render(&app).contains("Launch application"));
    }

    #[test]
    fn power_off_from_the_main_menu_asks_first() {
        let mut app = app().with_tessera(true);
        app.update(Event::Key(Key::char('p')));
        assert_eq!(app.update(Event::Key(Key::char('p'))), Vec::new());
        assert!(render(&app).contains("Turn the computer off?"));
        assert_eq!(
            app.update(Event::Key(Key::char('y'))),
            vec![Effect::Power(PowerAction::PowerOff)]
        );
    }

    #[test]
    fn esc_goes_back_but_never_past_the_main_menu() {
        let mut app = app();
        app.update(Event::Key(Key::char('p')));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.stack.len(), 1);
        assert!(!app.should_quit());
    }

    #[test]
    fn q_on_the_main_menu_quits() {
        let mut app = app();
        let effects = app.update(Event::Key(Key::char('q')));
        assert_eq!(effects, vec![Effect::Quit]);
        assert!(app.should_quit());
    }

    #[test]
    fn holding_enter_on_a_note_does_not_activate_the_menu_underneath() {
        // The bug: dismissing help with Enter, then auto-repeat activating the
        // selected entry (which opened a terminal).
        let mut app = app();
        press(&mut app, KeyCode::Down); // select "Terminal, tiled beside launcher"
        app.update(Event::Key(Key::char('?')));
        assert!(render(&app).contains("Esc"), "help should be open");

        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Vec::new(),
            "dismisses the note"
        );
        let repeat = app.update(Event::Key(Key::new(KeyCode::Enter).repeated()));
        assert_eq!(repeat, Vec::new(), "the repeat must not open a terminal");
        assert!(
            render(&app).contains("Launch application"),
            "back on the main menu"
        );
    }

    #[test]
    fn repeats_still_scroll_and_delete() {
        let mut app = app();
        app.update(Event::Key(Key::new(KeyCode::Down).repeated()));
        app.update(Event::Key(Key::new(KeyCode::Down).repeated()));
        match app.stack.last() {
            Some(Screen::Main { selected }) => assert_eq!(*selected, 2),
            _ => panic!("expected the main menu"),
        }
    }

    #[test]
    fn the_letters_in_the_buttons_work() {
        // <Exit> on the main menu.
        let mut app = app();
        assert_eq!(app.update(Event::Key(Key::char('x'))), vec![Effect::Quit]);

        // <Help> on the main menu.
        let mut app = App::new(Catalog::default(), ConfigValues::default());
        app.update(Event::Key(Key::char('h')));
        assert!(render(&app).contains("Esc"), "help should open");

        // <Back> on the windows screen, which does not filter as you type.
        let mut app = App::new(Catalog::default(), ConfigValues::default());
        app.update(Event::Key(Key::char('w')));
        app.set_windows(Vec::new());
        app.update(Event::Key(Key::char('b')));
        assert!(
            render(&app).contains("Launch application"),
            "back on the main menu"
        );
    }

    #[test]
    fn a_note_ignores_keys_that_do_not_mean_dismiss() {
        let mut app = app();
        app.update(Event::Key(Key::char('?'))); // the help note
        app.update(Event::Key(Key::char('l'))); // not a dismissal key
        assert!(
            render(&app).contains("Esc"),
            "the note should still be open"
        );
        press(&mut app, KeyCode::Esc);
        assert!(render(&app).contains("Launch application"));
    }

    #[test]
    fn question_mark_opens_help() {
        let mut app = app();
        app.update(Event::Key(Key::char('?')));
        assert!(render(&app).contains("Esc"), "help should mention Esc");
    }

    #[test]
    fn the_menu_says_when_it_is_running_outside_tessera() {
        let inside = render(&app().with_tessera(true));
        assert!(!inside.contains("not running under Tessera"), "{inside}");

        let outside = render(&app().with_tessera(false));
        assert!(outside.contains("not running under Tessera"), "{outside}");
    }

    fn window(id: u64, app_id: &str, title: &str, focused: bool) -> WindowInfo {
        WindowInfo {
            id: WindowId(id),
            app_id: Some(app_id.into()),
            title: Some(title.into()),
            workspace: 1,
            focused,
        }
    }

    #[test]
    fn the_windows_screen_asks_the_compositor_and_shows_the_answer() {
        let mut app = app();
        let effects = app.update(Event::Key(Key::char('w')));
        assert_eq!(effects, vec![Effect::ListWindows]);
        assert!(render(&app).contains("Asking the compositor"));

        app.set_windows(vec![
            window(1, "foot", "~/src", false),
            window(2, "firefox", "Wikipedia", true),
        ]);
        let screen = render(&app);
        assert!(screen.contains("foot"), "{screen}");
        assert!(screen.contains("Wikipedia"), "{screen}");
        assert!(screen.contains("2 windows"), "{screen}");
    }

    #[test]
    fn the_windows_screen_starts_on_the_focused_window_and_switches_to_it() {
        let mut app = app();
        app.update(Event::Key(Key::char('w')));
        app.set_windows(vec![
            window(1, "foot", "~/src", false),
            window(2, "firefox", "Wikipedia", true),
        ]);

        // Selection starts on the focused window, so Enter is a no-op switch...
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(effects, vec![Effect::Focus(WindowId(2)), Effect::Quit]);

        // ...and moving up first switches to the other one.
        let mut other = App::new(Catalog::default(), ConfigValues::default());
        other.update(Event::Key(Key::char('w')));
        other.set_windows(vec![
            window(1, "foot", "~/src", false),
            window(2, "firefox", "Wikipedia", true),
        ]);
        press(&mut other, KeyCode::Up);
        let effects = press(&mut other, KeyCode::Enter);
        assert_eq!(effects, vec![Effect::Focus(WindowId(1)), Effect::Quit]);
    }

    #[test]
    fn the_windows_screen_copes_with_no_windows() {
        let mut app = app();
        app.update(Event::Key(Key::char('w')));
        app.set_windows(Vec::new());
        assert!(render(&app).contains("No windows open"));
        assert_eq!(press(&mut app, KeyCode::Enter), Vec::new());
    }

    #[test]
    fn the_scripts_entry_opens_scripts_and_services() {
        let mut app = app().with_tessera(true);
        let effects = app.update(Event::Key(Key::char('s')));
        assert_eq!(effects, vec![Effect::ListScripts, Effect::QueryServices]);
        assert!(render(&app).contains("Scripts & services"));

        // Ticks reach the screen only while it is showing.
        app.update(Event::Key(Key::new(KeyCode::Esc)));
        assert_eq!(app.background(Update::Tick), Vec::new());
        assert_eq!(app.background(Update::Tick), Vec::new());
    }

    #[test]
    fn terminals_open_on_the_side_away_from_the_launcher() {
        let values = ConfigValues::parse("[launcher]\nside = \"right\"\n").unwrap();
        let mut app = App::new(Catalog::default(), values);
        let effects = app.update(Event::Key(Key::char('t')));
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                argv: vec!["foot".into()],
                placement: Placement::BesideCaller {
                    side: Side::Left,
                    ratio: 0.6,
                },
            }],
            "a launcher on the right opens terminals to its left"
        );
    }

    #[test]
    fn the_editor_is_one_that_exists() {
        let installed = |program: &str| ["vim", "nano"].contains(&program);
        let unset = |_: &str| None;
        let editor_var = |name: &str| (name == "EDITOR").then(|| "vim".to_string());

        assert_eq!(
            find_editor("nano", unset, installed),
            ["nano"],
            "the default"
        );
        assert_eq!(
            find_editor("nano -l", editor_var, installed),
            ["nano", "-l"],
            "the setting wins over $EDITOR"
        );
        assert_eq!(
            find_editor("kate", editor_var, installed),
            ["vim"],
            "a missing editor falls back to $EDITOR"
        );
        assert_eq!(
            find_editor("kate", unset, |program: &str| program == "vim"),
            ["vim"],
            "then to whatever common editor is installed"
        );
        assert!(find_editor("kate", unset, |_: &str| false).is_empty());
    }

    #[test]
    fn closing_the_window_quits() {
        let mut app = app();
        assert_eq!(app.update(Event::Closed), vec![Effect::Quit]);
        assert!(app.should_quit());
    }

    #[test]
    fn the_app_list_filters_as_you_type_and_launches() {
        let catalog = Catalog::from_apps(vec![
            crate::apps::App {
                name: "Firefox".into(),
                description: None,
                command: "firefox".into(),
                terminal: false,
            },
            crate::apps::App {
                name: "Calculator".into(),
                description: None,
                command: "kcalc".into(),
                terminal: false,
            },
        ]);
        let mut app = App::new(catalog, ConfigValues::default());

        app.update(Event::Key(Key::char('l')));
        assert!(render(&app).contains("Calculator"));

        app.update(Event::Key(Key::char('f')));
        app.update(Event::Key(Key::char('i')));
        let screen = render(&app);
        assert!(screen.contains("Firefox"), "{screen}");
        assert!(!screen.contains("Calculator"), "{screen}");
        assert!(screen.contains("Filter: fi"), "{screen}");

        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                argv: vec!["firefox".into()],
                placement: Placement::Auto,
            }]
        );
    }

    #[test]
    fn terminal_apps_are_launched_inside_a_terminal() {
        let catalog = Catalog::from_apps(vec![crate::apps::App {
            name: "htop".into(),
            description: None,
            command: "htop".into(),
            terminal: true,
        }]);
        let mut app = App::new(catalog, ConfigValues::default());
        app.update(Event::Key(Key::char('l')));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                argv: vec!["foot".into(), "-e".into(), "htop".into()],
                placement: Placement::Auto,
            }]
        );
    }

    fn two_apps() -> Catalog {
        Catalog::from_apps(vec![
            crate::apps::App {
                name: "Firefox".into(),
                description: None,
                command: "firefox".into(),
                terminal: false,
            },
            crate::apps::App {
                name: "Calculator".into(),
                description: None,
                command: "kcalc".into(),
                terminal: false,
            },
        ])
    }

    #[test]
    fn the_overlay_starts_on_the_app_list_already_filtering() {
        let mut app = App::apps_only(two_apps(), ConfigValues::default());
        let screen = render(&app);
        assert!(screen.contains("Launch application"), "{screen}");
        assert!(screen.contains("Filter: "), "{screen}");
        assert!(!screen.contains("<Back>"), "{screen}");

        // The first key filters: no `/` needed, and `f` is not a hotkey here.
        app.update(Event::Key(Key::char('c')));
        let screen = render(&app);
        assert!(screen.contains("Filter: c"), "{screen}");
        assert!(screen.contains("Calculator"), "{screen}");
        assert!(!screen.contains("Firefox"), "{screen}");
        assert!(!app.should_quit());
    }

    #[test]
    fn launching_from_the_overlay_spawns_tiled_the_usual_way_and_quits() {
        let mut app = App::apps_only(two_apps(), ConfigValues::default());
        app.update(Event::Key(Key::char('f')));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            vec![
                Effect::Spawn {
                    argv: vec!["firefox".into()],
                    placement: Placement::Auto,
                },
                Effect::Quit,
            ]
        );
        assert!(app.should_quit());
    }

    #[test]
    fn esc_closes_the_overlay_without_launching() {
        let mut app = App::apps_only(two_apps(), ConfigValues::default());
        app.update(Event::Key(Key::char('f')));
        assert_eq!(press(&mut app, KeyCode::Esc), vec![Effect::Quit]);
        assert!(app.should_quit());
    }

    #[test]
    fn a_filter_matching_nothing_says_so_and_stays_open() {
        let mut app = App::apps_only(two_apps(), ConfigValues::default());
        for ch in "zzz".chars() {
            app.update(Event::Key(Key::char(ch)));
        }
        assert_eq!(press(&mut app, KeyCode::Enter), Vec::new());
        assert!(!app.should_quit());
        assert!(render(&app).contains("nothing matches"), "{}", render(&app));
    }

    #[test]
    fn the_full_launcher_still_stays_open_after_launching() {
        let mut app = App::new(two_apps(), ConfigValues::default());
        app.update(Event::Key(Key::char('l')));
        app.update(Event::Key(Key::char('f')));
        let effects = press(&mut app, KeyCode::Enter);
        assert_eq!(effects.len(), 1, "{effects:?}");
        assert!(!app.should_quit());
    }
}
