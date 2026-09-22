//! The configuration menu, generated from the schema (design §6).
//!
//! Nothing here knows which settings exist: menus, markers, input boxes and
//! help all come from `tessera_config::Schema`. Adding a setting to
//! `schema.toml` makes it appear here with no UI code.
//!
//! Edits are made to an in-memory copy; `a` saves it to `config.toml` and asks
//! the compositor to reload, `l` throws the edits away and re-reads the file.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{ListItem, Paragraph},
};
use tessera_config::{ConfigValues, Entry, Kind, Pattern, Value};

use crate::{
    app::Effect,
    event::{Key, KeyCode},
    ui::{self, Tone},
};

/// What happened after a save, as reported by the front end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    /// Settings the compositor applied immediately.
    pub live: Vec<String>,
    /// Settings that wait for a restart.
    pub needs_restart: Vec<String>,
    /// False when there was no compositor to tell, so nothing was applied yet.
    pub reached_compositor: bool,
}

/// What the configuration menu wants the app to do after a key.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Keep showing the configuration menu.
    Stay,
    /// Leave the configuration menu.
    Close,
    /// Carry out an effect, then keep showing the menu.
    Effect(Effect),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Level {
    /// A menu of the schema; `None` is the top level.
    Menu { id: Option<String>, selected: usize },
    /// Typing a value for an int or string option.
    Input {
        key: String,
        text: String,
        error: Option<String>,
    },
    /// Picking one value of a choice option.
    Choice { key: String, selected: usize },
    /// Searching every option by name.
    Search { query: String, selected: usize },
    /// Explaining an entry.
    Help { title: String, body: String },
}

/// A visible line in a menu.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    entry: Entry,
    /// Shown only because the user asked to see hidden entries.
    hidden: bool,
}

/// The configuration menu's state, kept by the app between visits.
pub struct ConfigMenu {
    values: ConfigValues,
    saved: ConfigValues,
    levels: Vec<Level>,
    show_hidden: bool,
    restart_needed: Vec<String>,
    message: Option<(String, Tone)>,
}

impl ConfigMenu {
    /// Starts from the configuration as loaded from disk.
    pub fn new(values: ConfigValues) -> Self {
        Self {
            saved: values.clone(),
            values,
            levels: Vec::new(),
            show_hidden: false,
            restart_needed: Vec::new(),
            message: None,
        }
    }

    /// The configuration as last saved or loaded, which is what the launcher
    /// itself should act on; unsaved edits are not in effect anywhere.
    pub fn saved(&self) -> &ConfigValues {
        &self.saved
    }

    /// Whether there are edits that have not been saved.
    pub fn is_dirty(&self) -> bool {
        self.values.to_toml() != self.saved.to_toml()
    }

    /// Shows a message, e.g. that the file could not be loaded at startup.
    pub fn set_message(&mut self, text: impl Into<String>, tone: Tone) {
        self.message = Some((text.into(), tone));
    }

    /// Opens the menu at its top level.
    pub fn open(&mut self) {
        self.levels = vec![Level::Menu {
            id: None,
            selected: 0,
        }];
    }

    /// Handles one key.
    pub fn key(&mut self, key: Key) -> Outcome {
        let Some(level) = self.levels.last().cloned() else {
            return Outcome::Close;
        };
        match level {
            Level::Menu { id, selected } => self.key_in_menu(key, id, selected),
            Level::Input { key: option, .. } => self.key_in_input(key, &option),
            Level::Choice { key: option, .. } => self.key_in_choice(key, &option),
            Level::Search { .. } => self.key_in_search(key),
            Level::Help { .. } => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ' | 'q' | 'b' | 'x' | '?')
                ) {
                    self.levels.pop();
                }
                Outcome::Stay
            }
        }
    }

    /// Records the result of a save the front end carried out.
    pub fn save_finished(&mut self, result: Result<SaveReport, String>) {
        match result {
            Ok(report) => {
                self.saved = self.values.clone();
                for key in report.needs_restart {
                    if !self.restart_needed.contains(&key) {
                        self.restart_needed.push(key);
                    }
                }
                let text = if !report.reached_compositor {
                    "Saved. Not running under Tessera: it applies next start.".to_string()
                } else if report.live.is_empty() && self.restart_needed.is_empty() {
                    "Saved. Nothing had changed.".to_string()
                } else if self.restart_needed.is_empty() {
                    format!("Saved and applied ({} changed).", report.live.len())
                } else {
                    "Saved. Settings marked (restart) apply next start.".to_string()
                };
                self.message = Some((text, Tone::Info));
            }
            Err(reason) => self.message = Some((format!("Not saved: {reason}"), Tone::Error)),
        }
    }

    /// Records the result of re-reading the file.
    pub fn load_finished(&mut self, result: Result<ConfigValues, String>) {
        match result {
            Ok(values) => {
                self.saved = values.clone();
                self.values = values;
                self.message = Some(("Reloaded config.toml; edits discarded.".into(), Tone::Info));
            }
            Err(reason) => {
                self.message = Some((format!("Could not load: {reason}"), Tone::Error));
            }
        }
    }

    // ------------------------------------------------------------------ keys

    fn key_in_menu(&mut self, key: Key, id: Option<String>, selected: usize) -> Outcome {
        let rows = self.rows(id.as_deref());
        let selected = selected.min(rows.len().saturating_sub(1));
        let row = rows.get(selected).cloned();

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_menu(-1, rows.len()),
            KeyCode::Down | KeyCode::Char('j') => self.move_menu(1, rows.len()),
            KeyCode::PageUp => self.move_menu(-10, rows.len()),
            KeyCode::PageDown => self.move_menu(10, rows.len()),
            KeyCode::Enter | KeyCode::Char('s') | KeyCode::Right => {
                if let Some(row) = row {
                    self.activate(&row);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(Row {
                    entry: Entry::Option(index),
                    ..
                }) = row
                {
                    self.toggle(index, None);
                }
            }
            KeyCode::Char(ch @ ('y' | 'Y' | 'n' | 'N')) => {
                if let Some(Row {
                    entry: Entry::Option(index),
                    ..
                }) = row
                {
                    self.toggle(index, Some(ch.eq_ignore_ascii_case(&'y')));
                }
            }
            KeyCode::Char('?' | 'h') => {
                if let Some(row) = row {
                    self.levels.push(self.help_for(&row));
                }
            }
            KeyCode::Char('/') => self.levels.push(Level::Search {
                query: String::new(),
                selected: 0,
            }),
            KeyCode::Char('z' | 'Z') => {
                self.show_hidden = !self.show_hidden;
                let text = if self.show_hidden {
                    "Showing options hidden by their dependencies."
                } else {
                    "Hiding options whose dependencies are not met."
                };
                self.message = Some((text.into(), Tone::Info));
                let count = self.rows(id.as_deref()).len();
                self.move_menu(0, count);
            }
            KeyCode::Char('a' | 'A') => {
                return Outcome::Effect(Effect::SaveConfig {
                    toml: self.values.to_toml(),
                });
            }
            KeyCode::Char('l' | 'L') => return Outcome::Effect(Effect::LoadConfig),
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('x' | 'q' | 'b') => {
                self.levels.pop();
                if self.levels.is_empty() {
                    return Outcome::Close;
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn key_in_input(&mut self, key: Key, option: &str) -> Outcome {
        match key.code {
            KeyCode::Esc => {
                self.levels.pop();
            }
            KeyCode::Enter => {
                let text = match self.levels.last() {
                    Some(Level::Input { text, .. }) => text.clone(),
                    _ => return Outcome::Stay,
                };
                match self.values.set_from_text(option, &text) {
                    Ok(()) => {
                        self.levels.pop();
                        self.message = Some((format!("{option} = {text}"), Tone::Info));
                    }
                    Err(err) => {
                        if let Some(Level::Input { error, .. }) = self.levels.last_mut() {
                            *error = Some(err.reason);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(Level::Input { text, error, .. }) = self.levels.last_mut() {
                    text.pop();
                    *error = None;
                }
            }
            _ => {
                if let (Some(ch), Some(Level::Input { text, error, .. })) =
                    (key.typed(), self.levels.last_mut())
                {
                    text.push(ch);
                    *error = None;
                }
            }
        }
        Outcome::Stay
    }

    fn key_in_choice(&mut self, key: Key, option: &str) -> Outcome {
        let Some(Kind::Choice { choices }) = self
            .values
            .schema()
            .option(option)
            .map(|definition| definition.kind.clone())
        else {
            self.levels.pop();
            return Outcome::Stay;
        };
        let Some(Level::Choice { selected, .. }) = self.levels.last_mut() else {
            return Outcome::Stay;
        };

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(choices.len() - 1);
            }
            KeyCode::Enter | KeyCode::Char(' ' | 's') => {
                let choice = choices[(*selected).min(choices.len() - 1)].clone();
                self.levels.pop();
                if self.values.set(option, Value::Text(choice.clone())).is_ok() {
                    self.message = Some((format!("{option} = {choice}"), Tone::Info));
                }
            }
            KeyCode::Esc | KeyCode::Char('q' | 'b' | 'x') => {
                self.levels.pop();
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn key_in_search(&mut self, key: Key) -> Outcome {
        let Some(Level::Search { query, selected }) = self.levels.last().cloned() else {
            return Outcome::Stay;
        };
        let results = self.search(&query);

        match key.code {
            KeyCode::Esc => {
                self.levels.pop();
            }
            KeyCode::Up => self.set_search(query, selected.saturating_sub(1)),
            KeyCode::Down => {
                let last = results.len().saturating_sub(1);
                self.set_search(query, (selected + 1).min(last));
            }
            KeyCode::Backspace => {
                let mut query = query;
                query.pop();
                self.set_search(query, 0);
            }
            KeyCode::Enter => {
                if let Some(option) = results.get(selected) {
                    let option = option.clone();
                    self.levels.pop();
                    self.jump_to(&option);
                }
            }
            _ => {
                if let Some(ch) = key.typed() {
                    let mut query = query;
                    query.push(ch);
                    self.set_search(query, 0);
                }
            }
        }
        Outcome::Stay
    }

    // --------------------------------------------------------------- actions

    fn activate(&mut self, row: &Row) {
        let schema = self.values.schema();
        match row.entry {
            Entry::Menu(index) => self.levels.push(Level::Menu {
                id: Some(schema.menus[index].id.clone()),
                selected: 0,
            }),
            Entry::Option(index) => {
                let option = &schema.options[index];
                match &option.kind {
                    Kind::Bool => self.toggle(index, None),
                    Kind::Int { .. } | Kind::Text { .. } => {
                        let current = self.values.get(&option.key).map(|value| value.to_string());
                        self.levels.push(Level::Input {
                            key: option.key.clone(),
                            text: current.unwrap_or_default(),
                            error: None,
                        });
                    }
                    Kind::Choice { choices } => {
                        let current = self.values.text(&option.key);
                        self.levels.push(Level::Choice {
                            key: option.key.clone(),
                            selected: choices.iter().position(|c| *c == current).unwrap_or(0),
                        });
                    }
                }
            }
        }
    }

    /// Flips a bool (or sets it, for y/n). Forced options explain who forces them.
    fn toggle(&mut self, index: usize, to: Option<bool>) {
        let option = &self.values.schema().options[index];
        if option.kind != Kind::Bool {
            return;
        }
        if let Some(forcer) = self.values.forced_by(&option.key) {
            self.message = Some((format!("Forced on by {forcer}."), Tone::Error));
            return;
        }
        let next = to.unwrap_or(!self.values.bool(&option.key));
        let key = option.key.clone();
        if self.values.set(&key, Value::Bool(next)).is_ok() {
            self.message = None;
        }
    }

    fn move_menu(&mut self, delta: i32, count: usize) {
        if let Some(Level::Menu { selected, .. }) = self.levels.last_mut() {
            if count == 0 {
                *selected = 0;
                return;
            }
            *selected = (*selected as i32 + delta).clamp(0, count as i32 - 1) as usize;
        }
    }

    fn set_search(&mut self, query: String, selected: usize) {
        if let Some(Level::Search {
            query: current,
            selected: current_selected,
        }) = self.levels.last_mut()
        {
            *current = query;
            *current_selected = selected;
        }
    }

    /// Opens the menu holding `option` with it selected, from a search result.
    fn jump_to(&mut self, option: &str) {
        let schema = self.values.schema();
        let Some(index) = schema.options.iter().position(|o| o.key == option) else {
            return;
        };
        if !self.values.visible(option) {
            self.show_hidden = true;
        }
        let parent = schema.parent_of(option).map(str::to_string);
        let rows = self.rows(parent.as_deref());
        let selected = rows
            .iter()
            .position(|row| row.entry == Entry::Option(index))
            .unwrap_or(0);

        self.levels.truncate(1);
        if parent.is_some() {
            self.levels.push(Level::Menu {
                id: parent,
                selected,
            });
        } else if let Some(Level::Menu { selected: top, .. }) = self.levels.first_mut() {
            *top = selected;
        }
    }

    // -------------------------------------------------------------- queries

    fn rows(&self, menu: Option<&str>) -> Vec<Row> {
        let schema = self.values.schema();
        schema
            .entries(menu)
            .iter()
            .filter_map(|entry| {
                let visible = match entry {
                    Entry::Menu(index) => self.values.menu_visible(&schema.menus[*index].id),
                    Entry::Option(index) => self.values.visible(&schema.options[*index].key),
                };
                (visible || self.show_hidden).then(|| Row {
                    entry: entry.clone(),
                    hidden: !visible,
                })
            })
            .collect()
    }

    fn search(&self, query: &str) -> Vec<String> {
        let query = query.to_lowercase();
        self.values
            .schema()
            .options
            .iter()
            .filter(|option| {
                query.is_empty()
                    || option.prompt.to_lowercase().contains(&query)
                    || option.key.to_lowercase().contains(&query)
            })
            .map(|option| option.key.clone())
            .collect()
    }

    /// The menuconfig-style line for a row: `[*]`, `( )`, `(value)`, `--->`.
    fn label(&self, row: &Row) -> String {
        let schema = self.values.schema();
        match row.entry {
            Entry::Menu(index) => format!("    {}  --->", schema.menus[index].prompt),
            Entry::Option(index) => {
                let option = &schema.options[index];
                let restart = if self.restart_needed.contains(&option.key) {
                    "  (restart)"
                } else {
                    ""
                };
                match &option.kind {
                    Kind::Bool => {
                        let marker = if self.values.forced_by(&option.key).is_some() {
                            "-*-"
                        } else if self.values.bool(&option.key) {
                            "[*]"
                        } else {
                            "[ ]"
                        };
                        format!("{marker} {}{restart}", option.prompt)
                    }
                    Kind::Int { .. } | Kind::Text { .. } => {
                        let value = self
                            .values
                            .get(&option.key)
                            .map(|value| value.to_string())
                            .unwrap_or_default();
                        format!("({value}) {}{restart}", option.prompt)
                    }
                    Kind::Choice { .. } => format!(
                        "    {} ({})  --->{restart}",
                        option.prompt,
                        self.values.text(&option.key)
                    ),
                }
            }
        }
    }

    fn help_for(&self, row: &Row) -> Level {
        let schema = self.values.schema();
        match row.entry {
            Entry::Menu(index) => {
                let menu = &schema.menus[index];
                Level::Help {
                    title: menu.prompt.clone(),
                    body: if menu.help.is_empty() {
                        "Opens a submenu.".into()
                    } else {
                        menu.help.clone()
                    },
                }
            }
            Entry::Option(index) => {
                let option = &schema.options[index];
                let kind = match &option.kind {
                    Kind::Bool => "on or off".to_string(),
                    Kind::Int { min, max } => format!("a number from {min} to {max}"),
                    Kind::Text {
                        pattern: Some(Pattern::Binding),
                    } => "a key binding, e.g. Mod+Shift+Return".to_string(),
                    Kind::Text { pattern: None } => "text".to_string(),
                    Kind::Choice { choices } => format!("one of: {}", choices.join(", ")),
                };
                let applies = match option.apply {
                    tessera_config::Apply::Live => "as soon as you save",
                    tessera_config::Apply::Restart => "the next time the program starts",
                };
                let mut body = option.help.clone();
                body.push_str(&format!(
                    "\n\nSetting:   {}\nValue:     {kind}\nDefault:   {}\nTakes effect {applies}.",
                    option.key, option.default
                ));
                if let Some(expr) = &option.depends_on {
                    body.push_str(&format!("\nShown when: {expr}"));
                }
                Level::Help {
                    title: option.prompt.clone(),
                    body,
                }
            }
        }
    }

    // ------------------------------------------------------------------ view

    /// Draws whichever level is open.
    pub fn view(&self, frame: &mut Frame) {
        const HEADING: &str = "config.toml - Tessera Configuration";
        match self.levels.last() {
            Some(Level::Menu { id, selected }) => {
                self.view_menu(frame, HEADING, id.as_deref(), *selected)
            }
            Some(Level::Input { key, text, error }) => {
                self.view_input(frame, HEADING, key, text, error.as_deref());
            }
            Some(Level::Choice { key, selected }) => {
                self.view_choice(frame, HEADING, key, *selected)
            }
            Some(Level::Search { query, selected }) => {
                self.view_search(frame, HEADING, query, *selected);
            }
            Some(Level::Help { title, body }) => ui::draw_note(frame, HEADING, title, body),
            None => {}
        }
    }

    fn view_menu(&self, frame: &mut Frame, heading: &str, id: Option<&str>, selected: usize) {
        let schema = self.values.schema();
        let title = id
            .and_then(|id| schema.menu(id))
            .map(|menu| menu.prompt.as_str())
            .unwrap_or("Configuration");

        let area = ui::draw_screen(frame, heading);
        let dialog = ui::centred(area, 72, area.height.saturating_sub(2).min(26));
        let inner = ui::draw_dialog(frame, dialog, title);
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
                "Arrow keys navigate.  <Enter> selects submenus ---> and edits values.",
                "<Space>/<Y>/<N> toggle, </> searches, <?> explains, <Z> shows hidden.",
                "Legend: [*] on  [ ] off  (value)  -*- forced on  (restart) next start",
            ],
        );

        let entries = self.rows(id);
        let selected = selected.min(entries.len().saturating_sub(1));
        let items: Vec<ListItem> = entries
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let label = self.label(row);
                let line = if row.hidden {
                    ui::menu_line_dim(&label, index == selected)
                } else {
                    ui::menu_line(&label, None, index == selected)
                };
                ListItem::new(line)
            })
            .collect();

        let dirty_note = self
            .is_dirty()
            .then_some(("Unsaved changes: <a> saves, <l> discards.", Tone::Info));
        let status = self
            .message
            .as_ref()
            .map(|(text, tone)| (text.as_str(), *tone))
            .or(dirty_note);
        ui::draw_list(frame, rows[1], items, selected, status);

        ui::draw_buttons(
            frame,
            rows[2],
            &[
                ("Select", 'S'),
                ("Exit", 'x'),
                ("Help", 'H'),
                ("Save", 'a'),
                ("Load", 'L'),
            ],
            0,
        );
    }

    fn view_input(
        &self,
        frame: &mut Frame,
        heading: &str,
        key: &str,
        text: &str,
        error: Option<&str>,
    ) {
        let Some(option) = self.values.schema().option(key) else {
            return;
        };
        let constraint = match &option.kind {
            Kind::Int { min, max } => format!("A whole number from {min} to {max}."),
            Kind::Text {
                pattern: Some(Pattern::Binding),
            } => "Modifiers and a key, e.g. Mod+Shift+Return.".to_string(),
            _ => "Any text.".to_string(),
        };

        let area = ui::draw_screen(frame, heading);
        let dialog = ui::centred(area, 64, 11);
        let inner = ui::draw_dialog(frame, dialog, &option.prompt);
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(inner);

        ui::draw_help(
            frame,
            rows[0],
            &[&format!("Enter a value for {key}."), &constraint],
        );
        frame.render_widget(
            Paragraph::new(Line::from(format!(" > {text}▏"))).style(
                ratatui::style::Style::new()
                    .fg(ui::SELECTED_FG)
                    .bg(ui::SELECTED_BG),
            ),
            rows[1],
        );
        if let Some(error) = error {
            frame.render_widget(
                Paragraph::new(Line::from(error.to_string())).style(
                    ratatui::style::Style::new()
                        .fg(ui::HOTKEY_FG)
                        .bg(ui::DIALOG_BG)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                rows[2],
            );
        }
        ui::draw_buttons(frame, rows[4], &[("Ok", 'O'), ("Cancel", 'C')], 0);
    }

    fn view_choice(&self, frame: &mut Frame, heading: &str, key: &str, selected: usize) {
        let Some(option) = self.values.schema().option(key) else {
            return;
        };
        let Kind::Choice { choices } = &option.kind else {
            return;
        };
        let current = self.values.text(key);

        let area = ui::draw_screen(frame, heading);
        let dialog = ui::centred(area, 50, choices.len() as u16 + 9);
        let inner = ui::draw_dialog(frame, dialog, &option.prompt);
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        ui::draw_help(
            frame,
            rows[0],
            &["Choose one.  <Enter> selects, <Esc> cancels."],
        );
        let items: Vec<ListItem> = choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let marker = if *choice == current { "(X)" } else { "( )" };
                ListItem::new(ui::menu_line(
                    &format!("{marker} {choice}"),
                    None,
                    index == selected,
                ))
            })
            .collect();
        ui::draw_list(frame, rows[1], items, selected, None);
        ui::draw_buttons(frame, rows[2], &[("Select", 'S'), ("Cancel", 'C')], 0);
    }

    fn view_search(&self, frame: &mut Frame, heading: &str, query: &str, selected: usize) {
        let schema = self.values.schema();
        let results = self.search(query);

        let area = ui::draw_screen(frame, heading);
        let dialog = ui::centred(area, 72, area.height.saturating_sub(2).min(24));
        let inner = ui::draw_dialog(frame, dialog, "Search");
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        ui::draw_help(
            frame,
            rows[0],
            &[
                &format!("Search: {query}▏"),
                "<Enter> jumps to the setting, <Esc> cancels.",
            ],
        );
        let items: Vec<ListItem> = results
            .iter()
            .enumerate()
            .filter_map(|(index, key)| {
                let option = schema.option(key)?;
                let label = format!("{:<38} {}", option.prompt, option.key);
                Some(ListItem::new(ui::menu_line(
                    &label,
                    None,
                    index == selected,
                )))
            })
            .collect();
        let status = results
            .is_empty()
            .then_some(("No setting matches.", Tone::Info));
        ui::draw_list(frame, rows[1], items, selected, status);
        ui::draw_buttons(frame, rows[2], &[("Jump", 'J'), ("Cancel", 'C')], 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> ConfigMenu {
        let mut menu = ConfigMenu::new(ConfigValues::default());
        menu.open();
        menu
    }

    fn press(menu: &mut ConfigMenu, code: KeyCode) -> Outcome {
        menu.key(Key::new(code))
    }

    fn type_text(menu: &mut ConfigMenu, text: &str) {
        for ch in text.chars() {
            menu.key(Key::char(ch));
        }
    }

    /// Opens a top-level menu by its prompt and returns its row labels.
    fn open_menu(menu: &mut ConfigMenu, prompt: &str) {
        let rows = menu.rows(None);
        let index = rows
            .iter()
            .position(|row| menu.label(row).contains(prompt))
            .unwrap_or_else(|| panic!("no menu called {prompt}"));
        for _ in 0..index {
            press(menu, KeyCode::Down);
        }
        press(menu, KeyCode::Enter);
    }

    fn labels(menu: &ConfigMenu) -> Vec<String> {
        let Some(Level::Menu { id, .. }) = menu.levels.last() else {
            panic!("not in a menu");
        };
        menu.rows(id.as_deref())
            .iter()
            .map(|row| menu.label(row))
            .collect()
    }

    fn select(menu: &mut ConfigMenu, fragment: &str) {
        let labels = labels(menu);
        let index = labels
            .iter()
            .position(|label| label.contains(fragment))
            .unwrap_or_else(|| panic!("no row containing {fragment}: {labels:?}"));
        if let Some(Level::Menu { selected, .. }) = menu.levels.last_mut() {
            *selected = index;
        }
    }

    #[test]
    fn the_top_level_lists_the_schema_menus() {
        let menu = menu();
        let labels = labels(&menu);
        assert!(labels.iter().any(|label| label.contains("General  --->")));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Tiling layout  --->"))
        );
    }

    #[test]
    fn values_are_shown_with_menuconfig_markers() {
        let mut menu = menu();
        open_menu(&mut menu, "Tiling layout");
        let labels = labels(&menu);
        assert!(
            labels.contains(&"(8) Gap between windows (px)".to_string()),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"[ ] Snap tile sizes to cell grid".to_string()),
            "{labels:?}"
        );
    }

    #[test]
    fn space_toggles_a_bool_and_marks_the_menu_dirty() {
        let mut menu = menu();
        open_menu(&mut menu, "Tiling layout");
        select(&mut menu, "Snap tile sizes");
        press(&mut menu, KeyCode::Char(' '));
        assert!(labels(&menu).contains(&"[*] Snap tile sizes to cell grid".to_string()));
        assert!(menu.is_dirty());
        assert!(
            !menu.saved().bool("layout.snap_to_cells"),
            "saved copy is untouched"
        );
    }

    #[test]
    fn an_int_is_edited_in_an_input_box_that_refuses_bad_values() {
        let mut menu = menu();
        open_menu(&mut menu, "Tiling layout");
        select(&mut menu, "Gap between");
        press(&mut menu, KeyCode::Enter);
        assert!(matches!(menu.levels.last(), Some(Level::Input { .. })));

        // Clear the prefilled "8", then type an out-of-range value.
        press(&mut menu, KeyCode::Backspace);
        type_text(&mut menu, "999");
        press(&mut menu, KeyCode::Enter);
        match menu.levels.last() {
            Some(Level::Input {
                error: Some(error), ..
            }) => {
                assert!(error.contains("0–64"), "{error}");
            }
            other => panic!("the box should stay open with an error, got {other:?}"),
        }

        // Fix it: the box closes and the menu shows the new value.
        for _ in 0..3 {
            press(&mut menu, KeyCode::Backspace);
        }
        type_text(&mut menu, "24");
        press(&mut menu, KeyCode::Enter);
        assert!(labels(&menu).contains(&"(24) Gap between windows (px)".to_string()));
    }

    #[test]
    fn letters_typed_into_an_input_box_are_text_not_navigation() {
        let mut menu = menu();
        open_menu(&mut menu, "General");
        select(&mut menu, "Terminal command");
        press(&mut menu, KeyCode::Enter);
        for _ in 0.."foot".len() {
            press(&mut menu, KeyCode::Backspace);
        }
        type_text(&mut menu, "xq alacritty");
        match menu.levels.last() {
            Some(Level::Input { text, .. }) => assert_eq!(text, "xq alacritty"),
            other => panic!("still expected the input box, got {other:?}"),
        }
    }

    #[test]
    fn a_choice_opens_a_list_and_sets_the_value() {
        let mut menu = menu();
        open_menu(&mut menu, "General");
        select(&mut menu, "Modifier key");
        press(&mut menu, KeyCode::Enter);
        press(&mut menu, KeyCode::Down); // auto → alt
        press(&mut menu, KeyCode::Enter);
        assert_eq!(menu.values.text("general.mod_key"), "alt");
        assert!(
            labels(&menu)
                .iter()
                .any(|label| label.contains("Modifier key (alt)"))
        );
    }

    #[test]
    fn dependent_options_appear_when_their_dependency_is_met() {
        let mut menu = menu();
        open_menu(&mut menu, "Focus");
        assert!(
            !labels(&menu)
                .iter()
                .any(|label| label.contains("except over the launcher"))
        );

        select(&mut menu, "Focus follows mouse");
        press(&mut menu, KeyCode::Char('y'));
        assert!(
            labels(&menu)
                .iter()
                .any(|label| label.contains("except over the launcher"))
        );
    }

    #[test]
    fn z_reveals_hidden_options() {
        let mut menu = menu();
        open_menu(&mut menu, "Focus");
        press(&mut menu, KeyCode::Char('z'));
        let rows = {
            let Some(Level::Menu { id, .. }) = menu.levels.last() else {
                panic!()
            };
            menu.rows(id.as_deref())
        };
        assert!(
            rows.iter().any(|row| row.hidden),
            "a hidden row is now listed"
        );
    }

    #[test]
    fn search_jumps_to_the_setting_inside_its_menu() {
        let mut menu = menu();
        press(&mut menu, KeyCode::Char('/'));
        type_text(&mut menu, "gap between");
        press(&mut menu, KeyCode::Enter);

        let Some(Level::Menu { id, selected }) = menu.levels.last().cloned() else {
            panic!("expected to land in a menu");
        };
        assert_eq!(id.as_deref(), Some("layout"));
        assert!(labels(&menu)[selected].contains("Gap between windows"));
    }

    #[test]
    fn help_explains_type_default_and_when_it_applies() {
        let mut menu = menu();
        open_menu(&mut menu, "Launcher");
        select(&mut menu, "Font size");
        press(&mut menu, KeyCode::Char('?'));
        let Some(Level::Help { body, .. }) = menu.levels.last() else {
            panic!("expected help");
        };
        assert!(body.contains("from 8 to 48"), "{body}");
        assert!(body.contains("Default:   16"), "{body}");
        assert!(body.contains("next time"), "{body}");
    }

    #[test]
    fn saving_hands_the_toml_to_the_front_end_and_records_restarts() {
        let mut menu = menu();
        open_menu(&mut menu, "Launcher");
        select(&mut menu, "Font size");
        press(&mut menu, KeyCode::Enter);
        press(&mut menu, KeyCode::Backspace);
        press(&mut menu, KeyCode::Backspace);
        type_text(&mut menu, "20");
        press(&mut menu, KeyCode::Enter);

        let Outcome::Effect(Effect::SaveConfig { toml }) = press(&mut menu, KeyCode::Char('a'))
        else {
            panic!("a should save");
        };
        assert!(toml.contains("font_size = 20"), "{toml}");

        menu.save_finished(Ok(SaveReport {
            live: vec![],
            needs_restart: vec!["launcher.font_size".into()],
            reached_compositor: true,
        }));
        assert!(!menu.is_dirty());
        assert!(
            labels(&menu)
                .iter()
                .any(|label| label.contains("Font size (px)  (restart)")),
            "{:?}",
            labels(&menu)
        );
    }

    #[test]
    fn a_failed_save_keeps_the_edits_and_shows_why() {
        let mut menu = menu();
        open_menu(&mut menu, "Tiling layout");
        select(&mut menu, "Snap tile sizes");
        press(&mut menu, KeyCode::Char(' '));
        menu.save_finished(Err("permission denied".into()));
        assert!(menu.is_dirty(), "edits survive a failed save");
        let (text, tone) = menu.message.clone().unwrap();
        assert!(text.contains("permission denied"));
        assert_eq!(tone, Tone::Error);
    }

    #[test]
    fn leaving_the_top_level_closes_the_menu() {
        let mut menu = menu();
        open_menu(&mut menu, "General");
        assert_eq!(
            press(&mut menu, KeyCode::Esc),
            Outcome::Stay,
            "back to the top level"
        );
        assert_eq!(press(&mut menu, KeyCode::Esc), Outcome::Close);
    }
}
