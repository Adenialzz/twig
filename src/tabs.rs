//! Console 与 Agent 共用的居中圆角标签栏。
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph},
};

pub const HEADER_HEIGHT: u16 = 4;
const TAB_WIDTH: u16 = 11;
const TAB_HEIGHT: u16 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Console,
    Agent,
}

fn areas(area: Rect) -> [(Tab, &'static str, Rect); 2] {
    let width = area.width.min(TAB_WIDTH * 2 + 1);
    let x = area.x + (area.width - width) / 2;
    let tab_width = width.saturating_sub(1) / 2;
    [
        (
            Tab::Console,
            "Console",
            Rect::new(x, area.y, tab_width, area.height.min(TAB_HEIGHT)),
        ),
        (
            Tab::Agent,
            "Agent",
            Rect::new(
                x + tab_width + 1,
                area.y,
                tab_width,
                area.height.min(TAB_HEIGHT),
            ),
        ),
    ]
}

pub fn draw(frame: &mut Frame, area: Rect, active: Tab, hint: &str, agent_unread: bool) {
    for (tab, title, rect) in areas(area) {
        let style = if tab == active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title = if tab == Tab::Agent && agent_unread {
            "Agent*"
        } else {
            title
        };
        frame.render_widget(
            Paragraph::new(title)
                .alignment(Alignment::Center)
                .style(style)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded),
                ),
            rect,
        );
    }
    let right = areas(area)[1].2.right().saturating_add(2);
    let width = area.right().saturating_sub(right + 1);
    let hint = if Line::from(hint).width() <= usize::from(width) {
        hint
    } else {
        "Tab 切换"
    };
    if Line::from(hint).width() <= usize::from(width) && area.height >= TAB_HEIGHT {
        frame.render_widget(
            Paragraph::new(hint)
                .alignment(Alignment::Right)
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(right, area.y + 1, width, 1),
        );
    }
}

pub fn clicked(mouse: MouseEvent, area: Rect) -> Option<Tab> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    areas(area).into_iter().find_map(|(tab, _, rect)| {
        rect.contains((mouse.column, mouse.row).into())
            .then_some(tab)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn rounded_tabs_are_centered_and_clicks_follow_resize() {
        for width in [32, 65, 120, 180] {
            let area = Rect::new(0, 0, width, 20);
            let tabs = areas(area);
            assert!((i32::from(tabs[0].2.x) - i32::from(width - tabs[1].2.right())).abs() <= 1);
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            terminal
                .draw(|f| draw(f, area, Tab::Agent, "Tab 切换标签", true))
                .unwrap();
            for (tab, _, rect) in tabs {
                assert_eq!(terminal.backend().buffer()[(rect.x, 0)].symbol(), "╭");
                assert_eq!(terminal.backend().buffer()[(rect.x, 2)].symbol(), "╰");
                assert!(
                    clicked(
                        MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: rect.x + 3,
                            row: 1,
                            modifiers: KeyModifiers::NONE
                        },
                        area
                    ) == Some(tab)
                );
            }
            assert!(
                clicked(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: 0,
                        row: 1,
                        modifiers: KeyModifiers::NONE
                    },
                    area
                )
                .is_none()
            );
        }
    }
}
