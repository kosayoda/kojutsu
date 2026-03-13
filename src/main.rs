use std::time::Duration;

use color_eyre::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Flex, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

fn main() -> Result<()> {
    color_eyre::install()?;

    let mut terminal = jujujutsu::terminal::init()?;

    loop {
        terminal.draw(draw)?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != event::KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        }
    }

    jujujutsu::terminal::restore()?;
    Ok(())
}

fn draw(frame: &mut Frame) {
    let area = frame.area();

    let [center] = Layout::horizontal([Constraint::Min(0)])
        .flex(Flex::Center)
        .areas(area);
    let [_, middle, _, bottom, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(center);

    let title = Line::from(vec![Span::styled(
        "jujujutsu",
        Style::default().fg(Color::Magenta),
    )]);
    frame.render_widget(Paragraph::new(title).centered(), middle);

    let hint = Line::from(vec![
        Span::styled("press ", Style::default().fg(Color::DarkGray)),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::styled(" to quit", Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(hint).centered(), bottom);
}
