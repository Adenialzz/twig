use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionNotice {
    pub project: String,
    pub task: String,
}

/// 仅绘制覆盖层，不改变输入焦点或底层终端尺寸。
pub fn draw(frame: &mut Frame, notice: &CompletionNotice) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(48);
    let height = screen.height.saturating_sub(2).min(8);
    let area = Rect::new(
        screen.right().saturating_sub(width + 1),
        screen.y + 1,
        width,
        height,
    );
    let safe = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    let card = Paragraph::new(vec![
        Line::from(format!("项目：{}", safe(&notice.project))),
        Line::from(format!("任务：{}", safe(&notice.task))),
        Line::from(""),
        Line::from("返回 Console 查看任务"),
    ])
    .wrap(Wrap { trim: false })
    .style(Style::default().fg(Color::White).bg(Color::Indexed(24)))
    .block(
        Block::default()
            .title(" ✓ Codex 本轮已完成 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(card, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn card_is_top_right_and_preserves_footer_and_cursor() {
        for width in [38, 80, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| {
                    frame.render_widget(Paragraph::new("底部状态"), Rect::new(0, 23, width, 1));
                    frame.set_cursor_position((2, 12));
                    draw(
                        frame,
                        &CompletionNotice {
                            project: "demo".into(),
                            task: "notify".into(),
                        },
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| {
                let mut text = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    text.push_str(symbol);
                    x += Line::from(symbol).width().max(1) as u16;
                }
                text
            };
            assert!(row(1).contains("Codex 本轮已完成"));
            assert!(row(2).contains("项目：demo"));
            assert!(row(3).contains("任务：notify"));
            assert!(row(23).contains("底部状态"));
            assert_eq!(buffer[(width - 2, 1)].symbol(), "┓");
            assert_eq!(terminal.get_cursor_position().unwrap(), (2, 12).into());
            terminal
                .draw(|frame| {
                    frame.render_widget(Paragraph::new("终端内容已恢复"), frame.area());
                })
                .unwrap();
            assert_eq!(terminal.backend().buffer()[(width - 2, 1)].symbol(), " ");
        }
    }
}
