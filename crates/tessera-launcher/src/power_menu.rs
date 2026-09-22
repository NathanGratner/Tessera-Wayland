//! Power & session: log out, suspend, reboot, power off.
//!
//! Every choice asks "are you sure?" first, with **No** selected, because
//! these are the only launcher actions that cannot be undone: an Enter held a
//! moment too long must never power off a machine. Log out is a request to the
//! compositor; the rest go to logind through `systemctl`, on a worker thread,
//! falling back to a terminal when logind wants a password — the same path
//! service actions take.
//!
//! Like the other screens, this one does no I/O: keys and results go in,
//! [`Effect`]s come out.

use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    widgets::ListItem,
};
use tessera_ipc::{Placement, Side};
use tessera_services::{ActionError, PowerAction};

use crate::{
    app::Effect,
    event::{Key, KeyCode},
    ui::{self, Tone},
    worker::Update,
};

/// One thing this screen can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// End the Tessera session.
    LogOut,
    /// A logind power action.
    Power(PowerAction),
}

struct Entry {
    label: &'static str,
    hotkey: char,
    choice: Choice,
}

const ENTRIES: [Entry; 4] = [
    Entry {
        label: "Log out of Tessera",
        hotkey: 'L',
        choice: Choice::LogOut,
    },
    Entry {
        label: "Suspend",
        hotkey: 'S',
        choice: Choice::Power(PowerAction::Suspend),
    },
    Entry {
        label: "Reboot",
        hotkey: 'R',
        choice: Choice::Power(PowerAction::Reboot),
    },
    Entry {
        label: "Power off",
        hotkey: 'P',
        choice: Choice::Power(PowerAction::PowerOff),
    },
];

/// What the screen needs to know about its surroundings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// A compositor is there to log out of.
    pub under_tessera: bool,
    /// Tessera is a window inside another desktop, so power actions reach
    /// further than the person may expect.
    pub nested: bool,
    /// The terminal command, for the password fallback.
    pub terminal: Vec<String>,
    /// Share of the launcher's tile that terminal takes.
    pub share: f32,
    /// Which side of the launcher it opens on.
    pub side: Side,
}

/// What a key did to the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Still here; carry out these effects.
    Stay(Vec<Effect>),
    /// Leave the screen.
    Close,
}

/// The screen's state.
#[derive(Default)]
pub struct PowerMenu {
    ctx: Context,
    selected: usize,
    /// Asking "are you sure?" about this, and whether Yes is highlighted.
    confirming: Option<(Choice, bool)>,
    message: Option<(String, Tone)>,
}

impl PowerMenu {
    /// Shows the screen from the top.
    pub fn open(&mut self, ctx: Context) {
        self.ctx = ctx;
        self.selected = 0;
        self.confirming = None;
        self.message = None;
    }

    /// Handles a key press.
    pub fn key(&mut self, key: Key) -> Outcome {
        if let Some((choice, yes)) = self.confirming {
            return Outcome::Stay(self.key_in_confirm(key, choice, yes));
        }
        self.message = None;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(ENTRIES.len() - 1)
            }
            KeyCode::Enter => self.ask(ENTRIES[self.selected].choice),
            KeyCode::Esc | KeyCode::Char('b' | 'q') => return Outcome::Close,
            KeyCode::Char(ch) => {
                if let Some(index) = ENTRIES
                    .iter()
                    .position(|entry| entry.hotkey.eq_ignore_ascii_case(&ch))
                {
                    self.selected = index;
                    self.ask(ENTRIES[index].choice);
                }
            }
            _ => {}
        }
        Outcome::Stay(Vec::new())
    }

    fn ask(&mut self, choice: Choice) {
        if choice == Choice::LogOut && !self.ctx.under_tessera {
            self.message = Some(("No Tessera session to log out of".into(), Tone::Error));
            return;
        }
        // No is highlighted: Enter alone never confirms.
        self.confirming = Some((choice, false));
    }

    fn key_in_confirm(&mut self, key: Key, choice: Choice, yes: bool) -> Vec<Effect> {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.confirming = Some((choice, !yes));
                Vec::new()
            }
            KeyCode::Char('y' | 'Y') => self.carry_out(choice),
            KeyCode::Enter if yes => self.carry_out(choice),
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n' | 'N' | 'q' | 'b') => {
                self.confirming = None;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn carry_out(&mut self, choice: Choice) -> Vec<Effect> {
        self.confirming = None;
        match choice {
            Choice::LogOut => {
                self.message = Some(("ending the session…".into(), Tone::Info));
                vec![Effect::EndSession]
            }
            Choice::Power(action) => {
                let doing = match action {
                    PowerAction::Suspend => "suspending…",
                    PowerAction::Reboot => "rebooting…",
                    PowerAction::PowerOff => "powering off…",
                };
                self.message = Some((doing.into(), Tone::Info));
                vec![Effect::Power(action)]
            }
        }
    }

    /// The compositor refused to end the session, or could not be reached.
    pub fn report(&mut self, result: Result<(), String>) {
        if let Err(message) = result {
            self.message = Some((message, Tone::Error));
        }
    }

    /// A power action came back from logind.
    pub fn update(&mut self, update: Update) -> Vec<Effect> {
        let Update::PowerDone { action, result } = update else {
            return Vec::new();
        };
        match result {
            // A successful reboot or power-off never gets here in practice; a
            // suspend does, once the machine wakes, and needs no comment.
            Ok(()) => {
                self.message = None;
                Vec::new()
            }
            Err(ActionError::NeedsAuth) => {
                self.message = Some((
                    format!(
                        "{} needs a password: continue in the terminal",
                        action.as_str()
                    ),
                    Tone::Info,
                ));
                let mut argv = self.ctx.terminal.clone();
                argv.extend(["-e".into(), "systemctl".into(), action.as_str().into()]);
                vec![Effect::Spawn {
                    argv,
                    placement: Placement::BesideCaller {
                        side: self.ctx.side,
                        ratio: self.ctx.share,
                    },
                }]
            }
            Err(ActionError::Failed(reason)) => {
                self.message = Some((reason, Tone::Error));
                Vec::new()
            }
        }
    }

    /// Draws the list, or the question over it.
    pub fn view(&self, frame: &mut Frame, heading: &str) {
        match self.confirming {
            Some((choice, yes)) => self.view_confirm(frame, heading, choice, yes),
            None => self.view_list(frame, heading),
        }
    }

    fn view_list(&self, frame: &mut Frame, heading: &str) {
        let area = ui::draw_screen(frame, heading);
        let dialog = ui::centred(area, 56, ENTRIES.len() as u16 + 12);
        let inner = ui::draw_dialog(frame, dialog, "Power & session");
        let rows = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

        let mut help = vec![
            "Every choice asks before it does anything.",
            "Locking the screen needs a protocol Tessera",
            "does not implement yet, so it is not here.",
        ];
        if self.ctx.nested {
            help[0] = "Nested: power choices affect the whole computer.";
        }
        ui::draw_help(frame, rows[0], &help);

        let items: Vec<ListItem> = ENTRIES
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                ListItem::new(ui::menu_line(
                    entry.label,
                    Some(entry.hotkey),
                    index == self.selected,
                ))
            })
            .collect();
        let status = self
            .message
            .as_ref()
            .map(|(text, tone)| (text.as_str(), *tone));
        ui::draw_list(frame, rows[1], items, self.selected, status);

        ui::draw_buttons(frame, rows[2], &[("Select", 'S'), ("Back", 'B')], 0);
    }

    fn view_confirm(&self, frame: &mut Frame, heading: &str, choice: Choice, yes: bool) {
        let (title, mut lines) = match choice {
            Choice::LogOut if self.ctx.nested => (
                "Log out?",
                vec![
                    "Close this nested Tessera?",
                    "",
                    "Every program running in it will close.",
                ],
            ),
            Choice::LogOut => (
                "Log out?",
                vec![
                    "End the Tessera session?",
                    "",
                    "Every program running in it will close.",
                ],
            ),
            Choice::Power(PowerAction::Suspend) => ("Suspend?", vec!["Put the computer to sleep?"]),
            Choice::Power(PowerAction::Reboot) => (
                "Reboot?",
                vec![
                    "Restart the computer?",
                    "",
                    "Unsaved work in any program will be lost.",
                ],
            ),
            Choice::Power(PowerAction::PowerOff) => (
                "Power off?",
                vec![
                    "Turn the computer off?",
                    "",
                    "Unsaved work in any program will be lost.",
                ],
            ),
        };
        if self.ctx.nested && matches!(choice, Choice::Power(_)) {
            lines.extend([
                "",
                "Tessera is only a window here: this is the",
                "whole computer, not just that window.",
            ]);
        }

        let area = ui::draw_screen(frame, heading);
        let width = lines.iter().map(|line| line.len()).max().unwrap_or(20) as u16 + 8;
        let dialog = ui::centred(area, width.max(36), lines.len() as u16 + 6);
        let inner = ui::draw_dialog(frame, dialog, title);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
        ui::draw_help(frame, rows[0], &lines);
        ui::draw_buttons(
            frame,
            rows[1],
            &[("Yes", 'Y'), ("No", 'N')],
            if yes { 0 } else { 1 },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn ctx(nested: bool) -> Context {
        Context {
            under_tessera: true,
            nested,
            terminal: vec!["foot".into()],
            share: 0.6,
            side: Side::Right,
        }
    }

    fn menu(nested: bool) -> PowerMenu {
        let mut menu = PowerMenu::default();
        menu.open(ctx(nested));
        menu
    }

    fn press(menu: &mut PowerMenu, code: KeyCode) -> Vec<Effect> {
        match menu.key(Key::new(code)) {
            Outcome::Stay(effects) => effects,
            Outcome::Close => panic!("expected to stay"),
        }
    }

    fn render(menu: &PowerMenu) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 26)).unwrap();
        terminal.draw(|frame| menu.view(frame, "test")).unwrap();
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
    fn the_screen_lists_every_choice() {
        let screen = render(&menu(false));
        for label in ["Log out of Tessera", "Suspend", "Reboot", "Power off"] {
            assert!(screen.contains(label), "{label}: {screen}");
        }
        assert!(screen.contains("Locking"), "says why there is no lock");
    }

    #[test]
    fn enter_asks_first_and_no_is_the_default() {
        let mut menu = menu(false);
        assert_eq!(press(&mut menu, KeyCode::Char('p')), Vec::new());
        assert!(render(&menu).contains("Turn the computer off?"));
        assert_eq!(
            press(&mut menu, KeyCode::Enter),
            Vec::new(),
            "Enter on the default answer does nothing"
        );
        assert!(render(&menu).contains("Power off"), "back on the list");
    }

    #[test]
    fn yes_carries_it_out() {
        let mut menu = menu(false);
        press(&mut menu, KeyCode::Char('r'));
        assert_eq!(
            press(&mut menu, KeyCode::Char('y')),
            vec![Effect::Power(PowerAction::Reboot)]
        );

        let mut menu = self::menu(false);
        press(&mut menu, KeyCode::Char('p'));
        press(&mut menu, KeyCode::Left); // move to Yes
        assert_eq!(
            press(&mut menu, KeyCode::Enter),
            vec![Effect::Power(PowerAction::PowerOff)]
        );
    }

    #[test]
    fn holding_enter_cannot_power_off() {
        // Auto-repeat Enter never reaches here (the app drops it), but even a
        // second deliberate Enter only answers the default, which is No.
        let mut menu = menu(false);
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down);
        press(&mut menu, KeyCode::Down); // Power off
        press(&mut menu, KeyCode::Enter);
        assert_eq!(press(&mut menu, KeyCode::Enter), Vec::new());
    }

    #[test]
    fn logging_out_asks_the_compositor() {
        let mut menu = menu(false);
        press(&mut menu, KeyCode::Char('l'));
        assert!(render(&menu).contains("End the Tessera session?"));
        assert_eq!(
            press(&mut menu, KeyCode::Char('y')),
            vec![Effect::EndSession]
        );
    }

    #[test]
    fn logging_out_needs_a_compositor() {
        let mut menu = PowerMenu::default();
        menu.open(Context {
            under_tessera: false,
            ..ctx(false)
        });
        press(&mut menu, KeyCode::Char('l'));
        assert!(render(&menu).contains("No Tessera session to log out of"));
    }

    #[test]
    fn nested_says_the_whole_computer_is_affected() {
        let mut menu = menu(true);
        assert!(render(&menu).contains("whole computer"));
        press(&mut menu, KeyCode::Char('s'));
        let screen = render(&menu);
        assert!(screen.contains("Put the computer to sleep?"), "{screen}");
        assert!(screen.contains("whole computer"), "{screen}");
    }

    #[test]
    fn a_password_request_opens_a_terminal() {
        let mut menu = menu(false);
        let effects = menu.update(Update::PowerDone {
            action: PowerAction::Reboot,
            result: Err(ActionError::NeedsAuth),
        });
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                argv: vec![
                    "foot".into(),
                    "-e".into(),
                    "systemctl".into(),
                    "reboot".into()
                ],
                placement: Placement::BesideCaller {
                    side: Side::Right,
                    ratio: 0.6
                },
            }]
        );
        assert!(render(&menu).contains("needs a password"));
    }

    #[test]
    fn a_refusal_is_shown() {
        let mut menu = menu(false);
        menu.update(Update::PowerDone {
            action: PowerAction::Suspend,
            result: Err(ActionError::Failed("Sleep verb not supported".into())),
        });
        assert!(render(&menu).contains("Sleep verb not supported"));
    }

    #[test]
    fn esc_leaves_the_list_but_only_cancels_a_question() {
        let mut menu = menu(false);
        press(&mut menu, KeyCode::Char('p'));
        assert_eq!(menu.key(Key::new(KeyCode::Esc)), Outcome::Stay(Vec::new()));
        assert_eq!(menu.key(Key::new(KeyCode::Esc)), Outcome::Close);
    }
}
