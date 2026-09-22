//! The menuconfig look: blue ground, grey dialog, red hotkeys (design §5).

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, StatefulWidget,
        Widget,
    },
};

pub const SCREEN_BG: Color = Color::Blue;
pub const SCREEN_FG: Color = Color::White;
pub const DIALOG_BG: Color = Color::Gray;
pub const DIALOG_FG: Color = Color::Black;
pub const HOTKEY_FG: Color = Color::Red;
pub const SELECTED_BG: Color = Color::Blue;
pub const SELECTED_FG: Color = Color::White;
pub const SELECTED_HOTKEY_FG: Color = Color::LightYellow;
pub const DIM_FG: Color = Color::DarkGray;

pub fn dialog_style() -> Style {
    Style::new().bg(DIALOG_BG).fg(DIALOG_FG)
}

pub fn screen_style() -> Style {
    Style::new().bg(SCREEN_BG).fg(SCREEN_FG)
}

/// Paints the blue ground and the status line menuconfig shows along the top.
pub fn draw_screen(frame: &mut Frame, heading: &str) -> Rect {
    let area = frame.area();
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().style(screen_style()), area);

    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(format!(" {heading}")))
            .style(screen_style())
            .block(Block::new().borders(Borders::BOTTOM).style(screen_style())),
        rows[0],
    );
    rows[1]
}

/// Centres a dialog of the given size inside `area`, leaving room for its shadow.
pub fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(1)).max(1);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// Draws the grey dialog box with its title on the top border, and returns the
/// inner area. A one-cell shadow is painted down the right and bottom edges.
pub fn draw_dialog(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    let shadow = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width,
        height: area.height,
    };
    frame.render_widget(
        Block::new().style(Style::new().bg(Color::Black)),
        shadow.intersection(frame.area()),
    );

    let block = Block::bordered()
        .border_type(BorderType::Plain)
        .title(Line::from(format!(" {title} ")).centered())
        .title_style(Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .style(dialog_style());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

/// Renders help text in the dialog's own colours.
pub fn draw_help(frame: &mut Frame, area: Rect, lines: &[&str]) {
    let text: Vec<Line> = lines.iter().map(|line| Line::from(*line)).collect();
    frame.render_widget(Paragraph::new(text).style(dialog_style()), area);
}

/// One line of an inset list, with its hotkey letter highlighted.
pub fn menu_line(label: &str, hotkey: Option<char>, selected: bool) -> Line<'static> {
    let (fg, bg) = if selected {
        (SELECTED_FG, SELECTED_BG)
    } else {
        (DIALOG_FG, DIALOG_BG)
    };
    let hotkey_fg = if selected {
        SELECTED_HOTKEY_FG
    } else {
        HOTKEY_FG
    };
    let base = Style::new().fg(fg).bg(bg);

    let mut spans = vec![Span::styled(" ", base)];
    let mut highlighted = false;
    for ch in label.chars() {
        let is_hotkey = !highlighted && hotkey.is_some_and(|key| ch.eq_ignore_ascii_case(&key));
        if is_hotkey {
            highlighted = true;
            spans.push(Span::styled(
                ch.to_string(),
                Style::new()
                    .fg(hotkey_fg)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(ch.to_string(), base));
        }
    }
    Line::from(spans).style(base)
}

/// The `<Select> <Exit> <Help>` row along the bottom of a dialog.
pub fn draw_buttons(frame: &mut Frame, area: Rect, buttons: &[(&str, char)], selected: usize) {
    let mut spans = Vec::new();
    for (index, (label, hotkey)) in buttons.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", dialog_style()));
        }
        let chosen = index == selected;
        let (fg, bg) = if chosen {
            (SELECTED_FG, SELECTED_BG)
        } else {
            (DIALOG_FG, DIALOG_BG)
        };
        spans.push(Span::styled("<", Style::new().fg(fg).bg(bg)));
        for ch in label.chars() {
            let style = if ch.eq_ignore_ascii_case(hotkey) {
                Style::new()
                    .fg(if chosen {
                        SELECTED_HOTKEY_FG
                    } else {
                        HOTKEY_FG
                    })
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(fg).bg(bg)
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        spans.push(Span::styled(">", Style::new().fg(fg).bg(bg)));
    }

    let line = Line::from(spans).alignment(Alignment::Center);
    Paragraph::new(line)
        .style(dialog_style())
        .render(area, frame.buffer_mut());
}

/// A menu line for an entry that is hidden by an unmet dependency, shown
/// dimmed when the user asks to see everything.
pub fn menu_line_dim(label: &str, selected: bool) -> Line<'static> {
    let style = if selected {
        Style::new().fg(DIALOG_BG).bg(DIM_FG)
    } else {
        Style::new().fg(DIM_FG).bg(DIALOG_BG)
    };
    Line::from(Span::styled(format!(" {label}"), style)).style(style)
}

/// A status line under a list: normal, or an error in the hotkey red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Information.
    Info,
    /// Something went wrong and needs the user's attention.
    Error,
}

/// The inset list box used by every menu, with an optional status line
/// drawn over its bottom border.
pub fn draw_list(
    frame: &mut Frame,
    area: Rect,
    items: Vec<ListItem<'_>>,
    selected: usize,
    status: Option<(&str, Tone)>,
) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(DIM_FG).bg(DIALOG_BG))
        .style(dialog_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut state = ListState::default().with_selected(Some(selected));
    StatefulWidget::render(
        List::new(items).style(dialog_style()),
        inner,
        frame.buffer_mut(),
        &mut state,
    );

    if let Some((text, tone)) = status {
        let style = match tone {
            Tone::Info => dialog_style().add_modifier(Modifier::BOLD),
            Tone::Error => Style::new()
                .fg(HOTKEY_FG)
                .bg(DIALOG_BG)
                .add_modifier(Modifier::BOLD),
        };
        frame.render_widget(
            Paragraph::new(Line::from(format!(" {text} "))).style(style),
            Rect {
                y: area.y + area.height.saturating_sub(1),
                height: 1,
                ..area
            },
        );
    }
}

/// A dialog showing a block of text with a single Back button.
pub fn draw_note(frame: &mut Frame, heading: &str, title: &str, body: &str) {
    let area = draw_screen(frame, heading);
    let lines: Vec<&str> = body.lines().collect();
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20) as u16
        + 6;
    let dialog = centred(area, width.max(30), lines.len() as u16 + 5);
    let inner = draw_dialog(frame, dialog, title);

    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
    let text: Vec<Line> = lines.iter().map(|line| Line::from(*line)).collect();
    frame.render_widget(Paragraph::new(text).style(dialog_style()), rows[0]);
    draw_buttons(frame, rows[1], &[("Back", 'B')], 0);
}
