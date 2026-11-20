use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeState {
    Busy,
    Waiting,
    Idle,
    Done,
    Failed,
    Stopped,
}

impl ClaudeState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Busy => "运行中",
            Self::Waiting => "等待处理",
            Self::Idle | Self::Done => "空闲",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionNotice {
    pub claude: Option<ClaudeState>,
    pub message: bool,
    pub project: String,
    pub task: String,
}

impl CompletionNotice {
    pub fn same_task(&self, other: &Self) -> bool {
        self.project == other.project && self.task == other.task
    }
}

/// 仅绘制覆盖层，不改变输入焦点或底层终端尺寸。
pub fn draw(frame: &mut Frame, notice: &CompletionNotice) {
    let safe = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
    };
    let title = notice
        .claude
        .map(|state| format!(" Claude 状态变化 · {} ", state.label()));
    draw_card(
        frame,
        if let Some(title) = &title {
            title
        } else if notice.message {
            " Agent 收到新消息 "
        } else {
            " ✓ Codex 本轮已完成 "
        },
        vec![
            Line::from(format!("项目：{}", safe(&notice.project))),
            Line::from(format!("任务：{}", safe(&notice.task))),
            Line::from(""),
            Line::from("Ctrl-W g 跳转"),
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
            Line::from("Ctrl-W g 查看 Agent"),
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
                            claude: None,
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
            assert!(row(5).contains("Ctrl-W g 跳转"));
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

pub struct RecentTask {
    key: (String, String),
    boundary: Option<&'static str>,
}

impl RecentTask {
    pub fn new(key: (String, String), age_ms: u64) -> Self {
        let boundary = match age_ms / 86_400_000 {
            0 => Some("近一天"),
            1..=2 => Some("近三天"),
            3..=6 => Some("近一周"),
            _ => None,
        };
        Self { key, boundary }
    }
}

#[derive(Default)]
pub struct TaskPicker {
    selected: Option<CompletionNotice>,
    recent: Vec<RecentTask>,
    recent_error: Option<String>,
    recent_index: usize,
    mark_prefix: bool,
    feedback: Option<String>,
    showing_recent: bool,
    prefix: bool,
}

pub enum PickerAction {
    Stay,
    Close,
    Open(CompletionNotice),
    Recent(String, String),
    Mark(String, String),
}

enum ListAction {
    Stay,
    Close,
    Select(usize),
    Open(usize),
}

fn list_key(key: KeyEvent, prefix: &mut bool, index: Option<usize>, len: usize) -> ListAction {
    if key.kind == KeyEventKind::Release {
        return ListAction::Stay;
    }
    if key.code == KeyCode::Esc {
        return ListAction::Close;
    }
    if std::mem::take(prefix) {
        return if key.code == KeyCode::Char('u') {
            ListAction::Close
        } else {
            ListAction::Stay
        };
    }
    if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
        *prefix = true;
        return ListAction::Stay;
    }
    let Some(index) = index.filter(|i| *i < len) else {
        return ListAction::Stay;
    };
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => ListAction::Select((index + 1).min(len - 1)),
        KeyCode::Char('k') | KeyCode::Up => ListAction::Select(index.saturating_sub(1)),
        KeyCode::Enter => ListAction::Open(index),
        _ => ListAction::Stay,
    }
}

impl TaskPicker {
    pub fn new(recent: anyhow::Result<Vec<RecentTask>>) -> Self {
        match recent {
            Ok(recent) => Self {
                recent,
                ..Self::default()
            },
            Err(error) => Self {
                recent_error: Some(format!("读取最近任务失败：{error:#}")),
                ..Self::default()
            },
        }
    }

    pub fn sync(&mut self, tasks: &[UnreadTask]) {
        if !tasks.iter().any(|task| {
            self.selected
                .as_ref()
                .is_some_and(|selected| selected.same_task(&task.notice))
        }) {
            self.selected = tasks.first().map(|task| task.notice.clone());
        }
    }

    pub fn key(&mut self, key: KeyEvent, tasks: &[UnreadTask]) -> PickerAction {
        if key.kind == KeyEventKind::Release {
            return PickerAction::Stay;
        }
        self.feedback = None;
        if self.showing_recent
            && !self.prefix
            && key.code == KeyCode::Char('u')
            && key.modifiers.is_empty()
        {
            if !std::mem::take(&mut self.mark_prefix) {
                self.mark_prefix = true;
            } else if let Some(row) = self.recent.get(self.recent_index) {
                let (project, task) = row.key.clone();
                return PickerAction::Mark(project, task);
            }
            return PickerAction::Stay;
        }
        self.mark_prefix = false;
        if key.kind == KeyEventKind::Press && key.code == KeyCode::Tab && !self.prefix {
            self.showing_recent = !self.showing_recent;
            return PickerAction::Stay;
        }
        let (index, len) = if self.showing_recent {
            (Some(self.recent_index), self.recent.len())
        } else {
            (self.unread_index(tasks), tasks.len())
        };
        match list_key(key, &mut self.prefix, index, len) {
            ListAction::Close => PickerAction::Close,
            ListAction::Open(i) if self.showing_recent => {
                let (project, task) = self.recent[i].key.clone();
                PickerAction::Recent(project, task)
            }
            ListAction::Open(i) => PickerAction::Open(tasks[i].notice.clone()),
            ListAction::Select(i) => {
                if self.showing_recent {
                    self.recent_index = i;
                } else {
                    self.selected = Some(tasks[i].notice.clone());
                }
                PickerAction::Stay
            }
            ListAction::Stay => PickerAction::Stay,
        }
    }

    pub fn marked(&mut self, result: anyhow::Result<()>) {
        self.feedback = Some(match result {
            Ok(()) => "已标为未读".into(),
            Err(error) => format!("标记失败：{error:#}"),
        });
    }

    pub fn showing_recent(&self) -> bool {
        self.showing_recent
    }

    pub fn draw(
        &self,
        frame: &mut Frame,
        tasks: &[UnreadTask],
        running: Option<&std::collections::BTreeSet<(String, String)>>,
    ) {
        if self.showing_recent {
            let mut items = Vec::new();
            let mut selected = None;
            for (index, row) in self.recent.iter().enumerate() {
                if row.boundary.is_none()
                    && (index == 0 || self.recent[index - 1].boundary.is_some())
                {
                    items.push(
                        ListItem::new("── 更早 ──").style(Style::default().fg(Color::DarkGray)),
                    );
                }
                if index == self.recent_index {
                    selected = Some(items.len());
                }
                let (project, task) = &row.key;
                let label = if task == "main" {
                    "主工作目录"
                } else {
                    task
                };
                let mut line = Line::from(format!("{project} / {label}"));
                if running.is_some_and(|tasks| tasks.contains(&row.key)) {
                    line.push_span(" · ");
                    line.push_span(Span::styled(
                        "Codex 运行中",
                        Style::default().fg(Color::Cyan),
                    ));
                }
                items.push(ListItem::new(line));
                if let Some(label) = row.boundary
                    && self.recent.get(index + 1).and_then(|next| next.boundary) != Some(label)
                {
                    items.push(
                        ListItem::new(format!("── {label} ──"))
                            .style(Style::default().fg(Color::DarkGray)),
                    );
                }
            }
            draw_task_list(
                frame,
                " 未读任务  [最近任务] ",
                self.recent_error.as_deref().unwrap_or("暂无其他最近任务"),
                items,
                selected,
                self.feedback
                    .as_deref()
                    .unwrap_or(" Tab 切换 · j/k 选择 · Enter 进入 · Esc 关闭 · uu 标为未读 "),
            );
            return;
        }
        let items = tasks
            .iter()
            .map(|task| {
                let clean =
                    |text: &str| text.chars().filter(|c| !c.is_control()).collect::<String>();
                ListItem::new(format!(
                    "{} / {} / 未读 {}",
                    clean(&task.notice.project),
                    clean(&task.notice.task),
                    task.count
                ))
            })
            .collect::<Vec<_>>();
        draw_task_list(
            frame,
            " [未读任务]  最近任务 ",
            "暂无未读任务",
            items,
            self.unread_index(tasks),
            " Tab 切换 · j/k 选择 · Enter 进入 · Esc 关闭 ",
        );
    }

    fn unread_index(&self, tasks: &[UnreadTask]) -> Option<usize> {
        tasks.iter().position(|task| {
            self.selected
                .as_ref()
                .is_some_and(|selected| selected.same_task(&task.notice))
        })
    }
}

fn draw_task_list(
    frame: &mut Frame,
    title: &str,
    empty: &str,
    items: Vec<ListItem>,
    selected: Option<usize>,
    footer: &str,
) {
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
        .title(title)
        .title_bottom(footer)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().fg(Color::White).bg(Color::Black))
        .border_style(Style::default().fg(Color::Yellow));
    frame.render_widget(Clear, area);
    if items.is_empty() {
        frame.render_widget(Paragraph::new(empty).block(block), area);
        return;
    }
    let items = items.into_iter().enumerate().map(|(index, item)| {
        if Some(index) == selected {
            item.style(Style::default().fg(Color::Yellow))
        } else {
            item
        }
    });
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

#[cfg(test)]
mod picker_tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn recent_boundaries_follow_tasks_and_selection_skips_separators() {
        let day = 86_400_000;
        let ages = [day - 1, day, 3 * day, 7 * day];
        let mut picker = TaskPicker::new(Ok(ages
            .into_iter()
            .enumerate()
            .map(|(i, age)| RecentTask::new(("demo".into(), format!("task{i}")), age))
            .collect()));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        picker.key(key(KeyCode::Tab), &[]);
        picker.key(key(KeyCode::Down), &[]);
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, &[], None))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buffer.area.height {
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                screen.push_str(symbol);
                x += Line::from(symbol).width().max(1) as u16;
            }
            screen.push('\n');
        }
        let mut rest = screen.as_str();
        for label in [
            "task0",
            "近一天",
            "› demo / task1",
            "近三天",
            "task2",
            "近一周",
            "更早",
            "task3",
        ] {
            rest = rest
                .split_once(label)
                .unwrap_or_else(|| panic!("缺少 {label}: {screen}"))
                .1;
        }
        assert!(
            matches!(picker.key(key(KeyCode::Enter), &[]), PickerAction::Recent(_, task) if task == "task1")
        );
    }

    #[test]
    fn recent_running_status_keeps_cyan_on_both_row_backgrounds() {
        let tasks = vec![("demo".into(), "one".into()), ("demo".into(), "two".into())];
        let running = tasks.iter().cloned().collect();
        let mut picker = TaskPicker::new(Ok(tasks
            .into_iter()
            .map(|key| RecentTask::new(key, 0))
            .collect()));
        picker.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &[]);
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| picker.draw(frame, &[], Some(&running)))
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        for symbol in ["C", "运", "行", "中"] {
            let colors: Vec<_> = cells
                .iter()
                .filter(|cell| cell.symbol() == symbol)
                .map(|cell| (cell.fg, cell.bg))
                .collect();
            assert_eq!(
                colors,
                [(Color::Cyan, Color::DarkGray), (Color::Cyan, Color::Black)]
            );
        }
    }

    #[test]
    fn picker_keeps_task_selected_when_notice_type_changes() {
        let mut tasks = vec![task("first"), task("selected")];
        let mut picker = TaskPicker::default();
        picker.sync(&tasks);
        picker.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &tasks);
        tasks[1].notice.message = true;
        picker.sync(&tasks);
        assert!(
            matches!(picker.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tasks), PickerAction::Open(notice) if notice.task == "selected" && notice.message)
        );
    }

    fn task(name: &str) -> UnreadTask {
        UnreadTask {
            notice: CompletionNotice {
                claude: None,
                message: false,
                project: "demo".into(),
                task: name.into(),
            },
            count: 1,
        }
    }

    #[test]
    fn selection_tracks_identity_during_reordering_and_empty_updates() {
        let mut picker = TaskPicker::default();
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
            let mut picker = TaskPicker::default();
            terminal
                .draw(|frame| picker.draw(frame, &[], None))
                .unwrap();
            let tasks = (0..30)
                .map(|i| task(&format!("task{i}")))
                .collect::<Vec<_>>();
            picker.sync(&tasks);
            for _ in 0..29 {
                picker.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &tasks);
            }
            terminal
                .draw(|frame| picker.draw(frame, &tasks, None))
                .unwrap();
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
