//! Console 与 Agent 共用的单行标签栏。
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

pub const HEADER_HEIGHT: u16 = 1;
const BRAND: &str = " twig ";
const TAB_WIDTH: u16 = 9;
pub const HINT: &str = "Tab 切换标签";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Console,
    Agent,
}

fn areas(area: Rect) -> [(Tab, Rect); 2] {
    let x = area.x + BRAND.len() as u16 + 1;
    [
        (Tab::Console, Rect::new(x, area.y, TAB_WIDTH, 1)),
        (
            Tab::Agent,
            Rect::new(x + TAB_WIDTH + 1, area.y, TAB_WIDTH, 1),
        ),
    ]
    .map(|(tab, rect)| (tab, rect.intersection(area)))
}

/// 绘制标签与摘要；摘要过长时截断，空间不足时省略右侧提示。
pub fn draw(frame: &mut Frame, area: Rect, active: Tab, info: Line, agent_unread: bool) {
    let row = Rect { height: 1, ..area };
    frame.render_widget(
        Paragraph::new(BRAND).style(Style::default().add_modifier(Modifier::BOLD)),
        row,
    );
    for (tab, rect) in areas(row) {
        let (title, style) = match (tab, tab == active) {
            (_, true) => (
                if tab == Tab::Console {
                    "Console"
                } else {
                    "Agent"
                },
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            (Tab::Agent, false) if agent_unread => (
                "Agent*",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            (Tab::Agent, false) => ("Agent", Style::default().fg(Color::DarkGray)),
            (Tab::Console, false) => ("Console", Style::default().fg(Color::DarkGray)),
        };
        frame.render_widget(
            Paragraph::new(title)
                .alignment(Alignment::Center)
                .style(style),
            rect,
        );
    }
    let left = areas(row)[1].1.right() + 2;
    let width = row.right().saturating_sub(left + 1);
    let hint_width = Line::from(HINT).width() as u16;
    let info_width = if info.width() as u16 + hint_width + 2 <= width {
        frame.render_widget(
            Paragraph::new(HINT)
                .alignment(Alignment::Right)
                .style(Style::default().fg(Color::DarkGray)),
            Rect::new(left, row.y, width, 1),
        );
        width - hint_width - 2
    } else {
        width
    };
    if info.width() > usize::from(info_width) && info_width > 0 {
        // 末尾留一列放省略号，避免截断后的计数看起来像完整值。
        frame.render_widget(
            Paragraph::new("…"),
            Rect::new(left + info_width - 1, row.y, 1, 1),
        );
        frame.render_widget(
            Paragraph::new(info),
            Rect::new(left, row.y, info_width - 1, 1),
        );
    } else {
        frame.render_widget(Paragraph::new(info), Rect::new(left, row.y, info_width, 1));
    }
}

pub fn clicked(mouse: MouseEvent, area: Rect) -> Option<Tab> {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    areas(Rect { height: 1, ..area })
        .into_iter()
        .find_map(|(tab, rect)| {
            rect.contains((mouse.column, mouse.row).into())
                .then_some(tab)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    fn click(column: u16, row: u16, area: Rect) -> Option<Tab> {
        clicked(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            area,
        )
    }

    #[test]
    fn single_row_tabs_keep_clicks_and_truncate_info() {
        for width in [38, 65, 120, 180] {
            let area = Rect::new(0, 0, width, 20);
            let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
            let info = Line::from("1 个项目 · 2 个任务 · ".repeat(10));
            terminal
                .draw(|f| draw(f, area, Tab::Agent, info, true))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let row: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(row.starts_with(" twig "), "{row}");
            assert!(row.contains("Console") && row.contains("Agent"), "{row}");
            assert!(row.contains('…'), "{row}");
            assert!((0..width).all(|x| buffer[(x, 1)].symbol() == " "));
            for (tab, rect) in areas(area) {
                assert_eq!(click(rect.x + 2, 0, area), Some(tab));
                assert_eq!(click(rect.x + 2, 1, area), None);
            }
            assert_eq!(click(0, 0, area), None);
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 3)).unwrap();
        terminal
            .draw(|f| draw(f, f.area(), Tab::Console, Line::from("短摘要"), false))
            .unwrap();
        let row: String = (0..120)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect();
        let row: String = row.split_whitespace().collect();
        assert!(
            row.contains("短摘要") && row.ends_with("Tab切换标签"),
            "{row}"
        );
    }
}
