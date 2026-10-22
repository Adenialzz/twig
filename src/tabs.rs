//! Console 与 Chat 共用的顶部标签栏。
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Console,
    Chat,
}

fn areas(area: Rect) -> [(Tab, &'static str, Rect); 2] {
    [
        (
            Tab::Console,
            " Console ",
            Rect::new(area.x, area.y, area.width.min(9), area.height.min(1)),
        ),
        (
            Tab::Chat,
            " Chat ",
            Rect::new(
                area.x.saturating_add(10),
                area.y,
                area.width.saturating_sub(10).min(6),
                area.height.min(1),
            ),
        ),
    ]
}

pub fn draw(frame: &mut Frame, area: Rect, active: Tab, hint: &str, chat_unread: bool) {
    for (tab, title, rect) in areas(area) {
        let style = if tab == active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray).bg(Color::DarkGray)
        };
        let title = if tab == Tab::Chat && chat_unread {
            " Chat*"
        } else {
            title
        };
        frame.render_widget(Paragraph::new(title).style(style), rect);
    }
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        Rect::new(
            area.x.saturating_add(18),
            area.y,
            area.width.saturating_sub(18),
            area.height.min(1),
        ),
    );
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
