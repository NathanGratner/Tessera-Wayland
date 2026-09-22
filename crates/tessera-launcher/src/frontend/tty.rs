//! Terminal front end: the launcher running in any terminal, over ssh included.
//!
//! This is also how the UI is developed, since it needs no compositor.

use std::{io, sync::mpsc, time::Duration};

use anyhow::Context;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, KeyEventKind, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::App,
    event::{Event, Key, KeyCode, Mods, MouseKind},
    frontend::run_effects,
    ipc::Ipc,
    worker::{Update, Worker},
};

pub fn run(mut app: App) -> anyhow::Result<()> {
    enable_raw_mode().context("failed to put the terminal in raw mode")?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;

    let result = event_loop(
        &mut app,
        Terminal::new(CrosstermBackend::new(io::stdout()))?,
        Ipc::from_env(),
    );

    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture).ok();
    result
}

fn event_loop(
    app: &mut App,
    mut terminal: Terminal<CrosstermBackend<io::Stdout>>,
    ipc: Option<Ipc>,
) -> anyhow::Result<()> {
    let (sender, updates) = mpsc::channel::<Update>();
    let worker = Worker::new(move |update| {
        let _ = sender.send(update);
    });
    worker.start_ticking();
    if let Some(ipc) = &ipc {
        worker.subscribe(ipc);
    }

    loop {
        terminal.draw(|frame| app.view(frame))?;

        // Results from background work, then input. The short poll keeps
        // background results from waiting behind a quiet keyboard.
        while let Ok(update) = updates.try_recv() {
            let effects = app.background(update);
            run_effects(&effects, ipc.as_ref(), &worker).deliver(app);
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Some(event) = translate(event::read()?) else {
            continue;
        };
        let effects = app.update(event);
        run_effects(&effects, ipc.as_ref(), &worker).deliver(app);
        if app.should_quit() {
            return Ok(());
        }
    }
}

/// Converts a crossterm event into the launcher's own event type.
fn translate(event: event::Event) -> Option<Event> {
    match event {
        event::Event::Key(key) if key.kind != KeyEventKind::Release => {
            let code = match key.code {
                event::KeyCode::Char(ch) => KeyCode::Char(ch),
                event::KeyCode::Enter => KeyCode::Enter,
                event::KeyCode::Esc => KeyCode::Esc,
                event::KeyCode::Backspace => KeyCode::Backspace,
                event::KeyCode::Tab => KeyCode::Tab,
                event::KeyCode::Up => KeyCode::Up,
                event::KeyCode::Down => KeyCode::Down,
                event::KeyCode::Left => KeyCode::Left,
                event::KeyCode::Right => KeyCode::Right,
                event::KeyCode::Home => KeyCode::Home,
                event::KeyCode::End => KeyCode::End,
                event::KeyCode::PageUp => KeyCode::PageUp,
                event::KeyCode::PageDown => KeyCode::PageDown,
                event::KeyCode::Delete => KeyCode::Delete,
                _ => return None,
            };
            Some(Event::Key(Key {
                code,
                // crossterm reports repeats as their own kind.
                repeat: key.kind == KeyEventKind::Repeat,
                mods: Mods {
                    ctrl: key.modifiers.contains(event::KeyModifiers::CONTROL),
                    alt: key.modifiers.contains(event::KeyModifiers::ALT),
                    shift: key.modifiers.contains(event::KeyModifiers::SHIFT),
                },
            }))
        }
        event::Event::Mouse(mouse) => {
            let kind = match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => MouseKind::Press,
                MouseEventKind::ScrollUp => MouseKind::ScrollUp,
                MouseEventKind::ScrollDown => MouseKind::ScrollDown,
                _ => return None,
            };
            Some(Event::Mouse {
                col: mouse.column,
                row: mouse.row,
                kind,
            })
        }
        event::Event::Resize(cols, rows) => Some(Event::Resize(cols, rows)),
        _ => None,
    }
}
