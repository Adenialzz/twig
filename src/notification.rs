use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, List, ListState, Paragraph, Wrap},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionNotice {
    pub message: bool,
    pub project: String,
    pub task: String,
}

/// 仅绘制覆盖层，不改变输入焦点或底层终端尺寸。
pub fn draw(frame: &mut Frame, notice: &CompletionNotice) {
    let safe = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    draw_card(
        frame,
        if notice.message {
            " Agent 收到新消息 "
        } else {
            " ✓ Codex 本轮已完成 "
        },
        vec![
            Line::from(format!("项目：{}", safe(&notice.project))),
            Line::from(format!("任务：{}", safe(&notice.task))),
            Line::from(""),
            Line::from("Ctrl-W f 跳转"),
        ],
    );
}

pub fn draw_chat(frame: &mut Frame) {
    draw_card(
        frame,
        " 主控有新消息 ",
        vec![
            Line::from("主控收到消息或完成回复"),
            Line::from(""),
            Line::from("Ctrl-W f 查看 Agent"),
        ],
    );
}

fn draw_card(frame: &mut Frame, title: &str, lines: Vec<Line<'_>>) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(48);
    let height = screen.height.saturating_sub(2).min(8);
    let area = Rect::new(
        screen.right().saturating_sub(width + 1),
        screen.y + 1,
        width,
        height,
    );
    let card = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(Color::White).bg(Color::Indexed(24)))
        .block(
            Block::default()
                .title(title)
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
                            message: false,
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
            assert!(row(5).contains("Ctrl-W f 跳转"));
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadTask {
    pub notice: CompletionNotice,
    pub count: usize,
}

#[derive(Default)]
pub struct UnreadPicker {
    selected: Option<CompletionNotice>,
    prefix: bool,
}

pub enum PickerAction {
    Stay,
    Close,
    Open(CompletionNotice),
}

impl UnreadPicker {
    pub fn sync(&mut self, tasks: &[UnreadTask]) {
        if !tasks
            .iter()
            .any(|task| Some(&task.notice) == self.selected.as_ref())
        {
            self.selected = tasks.first().map(|task| task.notice.clone());
        }
    }

    pub fn key(&mut self, key: KeyEvent, tasks: &[UnreadTask]) -> PickerAction {
        if key.kind == KeyEventKind::Release {
            return PickerAction::Stay;
        }
        if key.code == KeyCode::Esc {
            return PickerAction::Close;
        }
        if self.prefix {
            self.prefix = false;
            return if key.code == KeyCode::Char('u') {
                PickerAction::Close
            } else {
                PickerAction::Stay
            };
        }
        if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.prefix = true;
            return PickerAction::Stay;
        }
        let Some(index) = tasks
            .iter()
            .position(|task| Some(&task.notice) == self.selected.as_ref())
        else {
            return PickerAction::Stay;
        };
        let next = match key.code {
            KeyCode::Char('j') | KeyCode::Down => (index + 1).min(tasks.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => index.saturating_sub(1),
            KeyCode::Enter => return PickerAction::Open(tasks[index].notice.clone()),
            _ => return PickerAction::Stay,
        };
        self.selected = Some(tasks[next].notice.clone());
        PickerAction::Stay
    }

    pub fn draw(&self, frame: &mut Frame, tasks: &[UnreadTask]) {
        let screen = frame.area();
        let width = screen.width.saturating_sub(2).min(76);
        let height = screen.height.saturating_sub(2).min(16);
        let area = Rect::new(
            screen.x + (screen.width - width) / 2,
            screen.y + (screen.height - height) / 2,
            width,
            height,
        );
        let block = Block::default()
            .title(" 未读任务 ")
            .title_bottom(" j/k 选择 · Enter 进入 · Esc 关闭 ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().fg(Color::White).bg(Color::Black))
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(Clear, area);
        if tasks.is_empty() {
            frame.render_widget(Paragraph::new("暂无未读任务").block(block), area);
            return;
        }
        let items = tasks
            .iter()
            .map(|task| {
                let clean =
                    |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
                format!(
                    "{} / {} / 未读 {}",
                    clean(&task.notice.project),
                    clean(&task.notice.task),
                    task.count
                )
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(
            tasks
                .iter()
                .position(|task| Some(&task.notice) == self.selected.as_ref()),
        );
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_symbol("› ")
                .highlight_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ),
            area,
            &mut state,
        );
    }
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn task(name: &str) -> UnreadTask {
        UnreadTask {
            notice: CompletionNotice {
                message: false,
                project: "demo".into(),
                task: name.into(),
            },
            count: 1,
        }
    }

    #[test]
    fn selection_tracks_identity_during_reordering_and_empty_updates() {
        let mut picker = UnreadPicker::default();
        let tasks = vec![task("one"), task("two")];
        picker.sync(&tasks);
        picker.key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &tasks,
        );
        let updated = vec![task("new"), task("two"), task("one")];
        picker.sync(&updated);
        assert!(
            matches!(picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &updated), PickerAction::Open(notice) if notice.task == "two")
        );
        picker.sync(&[task("remaining")]);
        assert_eq!(picker.selected.as_ref().unwrap().task, "remaining");
        picker.sync(&[]);
        assert!(matches!(
            picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &[]),
            PickerAction::Stay
        ));
        picker.key(
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
            &[],
        );
        assert!(matches!(
            picker.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE), &[]),
            PickerAction::Close
        ));
    }

    #[test]
    fn renders_empty_and_scrolls_selected_task_on_narrow_screens() {
        for width in [38, 80, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let mut picker = UnreadPicker::default();
            terminal.draw(|frame| picker.draw(frame, &[])).unwrap();
            let tasks = (0..30)
                .map(|i| task(&format!("task{i}")))
                .collect::<Vec<_>>();
            picker.sync(&tasks);
            for _ in 0..29 {
                picker.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &tasks);
            }
            terminal.draw(|frame| picker.draw(frame, &tasks)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("task29"));
        }
    }
}
