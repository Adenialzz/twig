use super::{Draft, View, render};
use crate::agent::{Delivery, MessageSource};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Wrap,
    },
};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    User,
    Assistant,
    Task,
    Completion,
    Unknown,
    Tool,
    Error,
}

pub(super) enum Tool {
    Command {
        status: String,
        exit_code: Option<i64>,
    },
    Files {
        status: String,
        changes: Vec<(String, String)>,
    },
}

pub(super) struct Entry {
    pub id: Option<String>,
    pub kind: Kind,
    pub text: String,
    pub detail: String,
    pub source_label: Option<String>,
    pub tool: Option<Tool>,
}
impl Entry {
    pub fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            text: clean(&text.into()),
            detail: String::new(),
            source_label: None,
            tool: None,
        }
    }
    pub fn apply_source(&mut self, message: &Delivery) {
        let (kind, label) = match &message.source {
            Some(MessageSource::User) => (Kind::User, None),
            Some(MessageSource::Task { project, task }) => {
                (Kind::Task, Some(format!("子任务 · {project}/{task}")))
            }
            Some(MessageSource::Completion { project, task }) => (
                Kind::Completion,
                Some(format!("轮次结束 · {project}/{task}")),
            ),
            None => (Kind::Unknown, None),
        };
        self.kind = kind;
        self.source_label = label.map(|label| clean(&label));
    }
}
pub(super) fn apply_sources(entries: &mut [Entry], messages: &[Delivery], thread: &str) {
    let sources: std::collections::HashMap<_, _> = messages
        .iter()
        .filter(|message| message.thread == thread)
        .map(|message| (message.id.as_str(), message))
        .collect();
    for entry in entries {
        if entry.kind == Kind::Unknown
            && let Some(message) = entry.id.as_deref().and_then(|id| sources.get(id))
        {
            entry.apply_source(message);
        }
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

pub(super) fn transcript(value: &Value) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(turns) = value["thread"]["turns"].as_array() {
        for turn in turns {
            if let Some(items) = turn["items"].as_array() {
                for item in items {
                    let entry = match item["type"].as_str() {
                        Some("userMessage") => {
                            let text = item["content"]
                                .as_array()
                                .map(|parts| {
                                    parts
                                        .iter()
                                        .filter_map(|part| part["text"].as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            let mut entry = Entry::new(Kind::Unknown, text);
                            entry.id = item["clientId"].as_str().map(str::to_owned);
                            entry
                        }
                        Some("agentMessage") => {
                            Entry::new(Kind::Assistant, item["text"].as_str().unwrap_or_default())
                        }
                        Some("commandExecution") => {
                            let mut entry = Entry::new(
                                Kind::Tool,
                                item["command"].as_str().unwrap_or_default(),
                            );
                            entry.detail =
                                clean(item["aggregatedOutput"].as_str().unwrap_or_default());
                            entry.tool = Some(Tool::Command {
                                status: item["status"].as_str().unwrap_or_default().to_owned(),
                                exit_code: item["exitCode"].as_i64(),
                            });
                            entry
                        }
                        Some("fileChange") => {
                            let changes = item["changes"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|change| {
                                    let kind = match change["kind"]["type"].as_str() {
                                        Some("add") => "新增",
                                        Some("delete") => "删除",
                                        _ => "修改",
                                    };
                                    (
                                        kind.to_owned(),
                                        clean(change["path"].as_str().unwrap_or_default()),
                                    )
                                })
                                .collect();
                            let mut entry = Entry::new(Kind::Tool, "文件修改");
                            entry.tool = Some(Tool::Files {
                                status: item["status"].as_str().unwrap_or_default().to_owned(),
                                changes,
                            });
                            entry
                        }
                        _ => continue,
                    };
                    entries.push(entry);
                }
            }
            if !turn["error"].is_null() {
                entries.push(Entry::new(
                    Kind::Error,
                    turn["error"]["message"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| turn["error"].to_string()),
                ));
            }
        }
    }
    entries
}

fn tool_lines(entry: &Entry, expanded: bool, width: usize) -> Vec<Line<'static>> {
    let Some(tool) = &entry.tool else {
        return render::plain(&entry.text, width, Style::default());
    };
    let status = match tool {
        Tool::Command { status, .. } | Tool::Files { status, .. } => status.as_str(),
    };
    let (symbol, label, color) = match status {
        "completed" => ("✓", "完成", Color::DarkGray),
        "inProgress" => ("●", "执行中", Color::Cyan),
        "failed" => ("×", "失败", Color::Red),
        "declined" => ("−", "已拒绝", Color::Yellow),
        _ => ("○", "待确认", Color::DarkGray),
    };
    let style = Style::default().fg(color);
    let mut lines = Vec::new();
    match tool {
        Tool::Command { exit_code, .. } => {
            let exit = exit_code
                .map(|code| format!(" · 退出码 {code}"))
                .unwrap_or_default();
            lines.extend(render::plain(
                &format!("{symbol} {label} · 命令{exit}"),
                width,
                style,
            ));
            let command = render::plain(&entry.text, width.saturating_sub(2), Style::default());
            append_preview(&mut lines, command, expanded, 2, false);
            if !entry.detail.is_empty() {
                let output = render::plain(&entry.detail, width.saturating_sub(2), style);
                append_preview(&mut lines, output, expanded, 3, status == "failed");
            }
        }
        Tool::Files { changes, .. } => {
            lines.extend(render::plain(
                &format!("{symbol} {label} · 文件修改 · {} 个文件", changes.len()),
                width,
                style,
            ));
            for (kind, path) in changes
                .iter()
                .take(if expanded { changes.len() } else { 3 })
            {
                lines.extend(render::plain(
                    &format!("  {kind} {path}"),
                    width,
                    Style::default(),
                ));
            }
            if !expanded && changes.len() > 3 {
                lines.extend(render::plain(
                    &format!("  … 另 {} 个文件 · F4 展开", changes.len() - 3),
                    width,
                    style,
                ));
            }
        }
    }
    lines
}

fn append_preview(
    lines: &mut Vec<Line<'static>>,
    content: Vec<Line<'static>>,
    expanded: bool,
    limit: usize,
    tail: bool,
) {
    let hidden = if expanded {
        0
    } else {
        content.len().saturating_sub(limit)
    };
    let marker = || {
        Line::styled(
            format!("  … 省略 {hidden} 行 · F4 展开"),
            Style::default().fg(Color::DarkGray),
        )
    };
    if tail && hidden > 0 {
        lines.push(marker());
    }
    let skip = if tail { hidden } else { 0 };
    let take = content.len() - hidden;
    for line in content.into_iter().skip(skip).take(take) {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(line.spans);
        lines.push(Line::from(spans).style(line.style));
    }
    if !tail && hidden > 0 {
        lines.push(marker());
    }
}

fn history(view: &View, draft: &Draft, width: usize) -> Text<'static> {
    let mut lines = Vec::new();
    for entry in &view.entries {
        if entry.kind == Kind::Tool {
            lines.extend(tool_lines(entry, draft.details, width));
            lines.push(Line::from(""));
            continue;
        }
        let (label, color) = match entry.kind {
            Kind::User => ("你", Color::Cyan),
            Kind::Assistant => ("主控", Color::Green),
            Kind::Task => ("子任务", Color::Yellow),
            Kind::Completion => ("轮次结束", Color::DarkGray),
            Kind::Unknown => ("来源未知", Color::Yellow),
            Kind::Tool => ("工具", Color::DarkGray),
            Kind::Error => ("需要处理", Color::Red),
        };
        let label = entry.source_label.as_deref().unwrap_or(label);
        lines.push(Line::styled(
            format!("▎ {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        if entry.kind == Kind::Assistant {
            lines.extend(render::markdown(&entry.text, width));
        } else {
            lines.extend(render::plain(&entry.text, width, Style::default()));
        }
        if draft.details && !entry.detail.is_empty() {
            lines.extend(render::plain(
                &entry.detail,
                width,
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(""));
    }
    for (label, text, color) in [
        ("连接 / 投递错误", &view.error, Color::Red),
        ("需要用户处理 · 原生 Codex", &view.approval, Color::Yellow),
    ] {
        if let Some(text) = text {
            lines.push(Line::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
            lines.extend(render::plain(&clean(text), width, Style::default()));
            lines.push(Line::from(""));
        }
    }
    if draft.details && !view.thread.is_empty() {
        lines.push(Line::styled(
            format!("会话 {}", view.thread),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Text::from(lines)
}

// 按终端列宽折行，渲染与光标共享同一布局，中文和多行输入不会错位。
fn input_lines(input: &str, cursor: usize, width: u16) -> (Vec<Line<'static>>, (u16, u16)) {
    let width = usize::from(width.max(2));
    let mut lines = vec![String::new()];
    let mut column = 0;
    let mut position = (0, 0);
    for (index, c) in input.char_indices() {
        let cell_width = Line::from(c.to_string()).width();
        if c != '\n' && column + cell_width > width {
            lines.push(String::new());
            column = 0;
        }
        if index == cursor {
            position = (column as u16, (lines.len() - 1) as u16);
        }
        if c == '\n' {
            lines.push(String::new());
            column = 0;
        } else {
            lines.last_mut().unwrap().push(c);
            column += cell_width;
        }
    }
    if column == width {
        lines.push(String::new());
        column = 0;
    }
    if cursor == input.len() {
        position = (column as u16, (lines.len() - 1) as u16);
    }
    (lines.into_iter().map(Line::from).collect(), position)
}

#[derive(Default)]
pub(super) struct Areas {
    pub history: Rect,
    pub input: Rect,
}

pub(super) fn draw(frame: &mut Frame, view: &View, draft: &mut Draft, show_cursor: bool) -> Areas {
    let screen = frame.area();
    if !view.connected && view.error.is_none() {
        frame.render_widget(
            Paragraph::new("连接中…")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Cyan)),
            Rect::new(
                screen.x,
                screen.y + screen.height / 2,
                screen.width,
                screen.height.min(1),
            ),
        );
        return Areas::default();
    }
    if screen.width < 32 || screen.height < 12 {
        frame.render_widget(
            Paragraph::new("请扩大终端到至少 32×12\nTab 返回 Console · Ctrl-C 清空/退出"),
            screen,
        );
        return Areas::default();
    }
    let width = screen.width.saturating_sub(4);
    let body = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + crate::tabs::HEADER_HEIGHT + 1,
        width,
        screen.height - crate::tabs::HEADER_HEIGHT - 1,
    );
    let (input, (cx, cy)) = input_lines(&draft.text, draft.cursor, width.saturating_sub(4));
    let input_height = (input.len() as u16)
        .clamp(1, 6)
        .min(screen.height.saturating_sub(11))
        + 2;
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(body);
    crate::tabs::draw(frame, screen, crate::tabs::Tab::Agent, "", false);
    frame.render_widget(
        Paragraph::new("YOLO · 无需审批 · 完整访问权限")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Rect::new(
            screen.x,
            screen.y + crate::tabs::HEADER_HEIGHT,
            screen.width,
            1,
        ),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" 主控  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                &view.status,
                Style::default().fg(if view.error.is_some() {
                    Color::Red
                } else if view.connected {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
            Span::styled(
                format!(
                    " · {} · 推理 {}",
                    view.model.as_deref().unwrap_or("模型未知"),
                    view.reasoning_effort.as_deref().unwrap_or("未指定")
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        Rect::new(body.x, screen.y + crate::tabs::HEADER_HEIGHT - 1, width, 1),
    );
    let transcript = Paragraph::new(history(
        view,
        draft,
        usize::from(rows[0].width.saturating_sub(2)),
    ))
    .wrap(Wrap { trim: false });
    let height = transcript
        .line_count(rows[0].width.saturating_sub(2))
        .min(u16::MAX as usize) as u16;
    if draft.scroll > 0 && draft.history_height > 0 && height > draft.history_height {
        draft.scroll = draft.scroll.saturating_add(height - draft.history_height);
    }
    draft.history_height = height;
    let bottom = height.saturating_sub(rows[0].height);
    draft.scroll = draft.scroll.min(bottom);
    let offset = bottom.saturating_sub(draft.scroll);
    let history_area = Rect::new(rows[0].x + 1, rows[0].y, rows[0].width - 2, rows[0].height);
    frame.render_widget(transcript.scroll((offset, 0)), history_area);
    if bottom > 0 {
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            rows[0],
            &mut ScrollbarState::new(usize::from(bottom) + 1).position(usize::from(offset)),
        );
    }
    frame.render_widget(
        Paragraph::new(if draft.scroll > 0 {
            "浏览历史 · Ctrl-End 回到最新"
        } else {
            "滚轮 / PgUp 浏览历史 · 拖选文字复制"
        })
        .style(Style::default().fg(Color::DarkGray)),
        rows[1],
    );
    let border = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if view.connected {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title(" 消息 ")
        .title_bottom(Line::from(format!(" {} 字 ", draft.text.chars().count())).right_aligned());
    let inner = border.inner(rows[2]);
    frame.render_widget(border, rows[2]);
    let input_area = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let start = cy.saturating_sub(input_area.height.saturating_sub(1));
    if draft.text.is_empty() {
        frame.render_widget(
            Paragraph::new("告诉主控你想完成什么…").style(Style::default().fg(Color::DarkGray)),
            input_area,
        );
    } else {
        frame.render_widget(Paragraph::new(input).scroll((start, 0)), input_area);
    }
    if show_cursor {
        frame.set_cursor_position((
            input_area.x + cx.min(input_area.width.saturating_sub(1)),
            input_area.y + cy.saturating_sub(start),
        ));
    }
    frame.render_widget(
        Paragraph::new(format!(
            "Enter 发送 · Alt-Enter 换行 · Alt-C 复制 · F4 {}详情",
            if draft.details { "收起" } else { "展开" }
        ))
        .style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
    Areas {
        history: history_area,
        input: input_area,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn command_failure_is_one_entry_with_literal_tail_and_expandable_output() {
        let output = "first\nsecond\nthird\nfourth\n**actual error**";
        let entries = transcript(&json!({"thread":{"turns":[{"items":[{
            "type":"commandExecution", "status":"failed", "command":"cargo test", "exitCode":101, "aggregatedOutput":output
        }]}]}}));
        assert_eq!(entries.len(), 1);
        let compact = tool_lines(&entries[0], false, 60)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(compact.contains("失败") && compact.contains("101"));
        assert!(compact.contains("**actual error**") && compact.contains("省略 2 行"));
        assert!(!compact.contains("first"));
        let full = tool_lines(&entries[0], true, 60)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full.contains("first") && !full.contains("省略"));
    }

    #[test]
    fn file_changes_show_paths_and_status_without_opening_details() {
        let entries = transcript(&json!({"thread":{"turns":[{"items":[{
            "type":"fileChange", "status":"completed", "changes":[
                {"path":"src/new.rs", "kind":{"type":"add"}},
                {"path":"src/old.rs", "kind":{"type":"delete"}}
            ]
        }]}]}}));
        let text = tool_lines(&entries[0], false, 60)
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        assert!(
            text.contains("2 个文件")
                && text.contains("新增 src/new.rs")
                && text.contains("删除 src/old.rs")
        );
    }

    #[test]
    fn command_preview_uses_available_width_instead_of_fixed_char_count() {
        let mut entry = Entry::new(Kind::Tool, "中".repeat(100));
        entry.tool = Some(Tool::Command {
            status: "inProgress".into(),
            exit_code: None,
        });
        let wide = tool_lines(&entry, false, 120);
        let narrow = tool_lines(&entry, false, 40);
        assert!(!wide.iter().any(|line| line.to_string().contains("省略")));
        assert!(narrow.iter().any(|line| line.to_string().contains("省略")));
        assert!(narrow.iter().all(|line| line.width() <= 40));
    }

    #[test]
    fn roles_follow_persisted_source_and_message_id_not_text() {
        let mut messages = Vec::new();
        for (id, source) in [
            ("user", Some(MessageSource::User)),
            (
                "task",
                Some(MessageSource::Task {
                    project: "项目".into(),
                    task: "任务".into(),
                }),
            ),
            (
                "event",
                Some(MessageSource::Completion {
                    project: "项目".into(),
                    task: "任务".into(),
                }),
            ),
            ("old", None),
        ] {
            messages.push(Delivery {
                id: id.into(),
                thread: "controller".into(),
                text: "任务反馈：相同正文".into(),
                source,
                status: "sent".into(),
                turn: None,
                error: None,
            });
        }
        let serialized = serde_json::to_string(&messages).unwrap();
        let mut messages: Vec<Delivery> = serde_json::from_str(&serialized).unwrap();
        let items: Vec<_> = messages.iter().map(|message| json!({"type":"userMessage", "clientId":message.id, "content":[{"text":message.text}]})).collect();
        let mut entries = transcript(&json!({"thread":{"turns":[{"items":items}]}}));
        let mut unrelated = messages[0].clone();
        unrelated.thread = "other".into();
        unrelated.source = messages[1].source.clone();
        messages.push(unrelated);
        apply_sources(&mut entries, &messages, "controller");
        let view = View {
            entries,
            ..View::default()
        };
        let rendered = history(&view, &Draft::default(), 80).to_string();
        for label in [
            "▎ 你",
            "▎ 子任务 · 项目/任务",
            "▎ 轮次结束 · 项目/任务",
            "▎ 来源未知",
        ] {
            assert_eq!(rendered.matches(label).count(), 1, "{rendered}");
        }
        for (entry, message) in view.entries.iter().zip(&messages) {
            let mut queued = Entry::new(Kind::Unknown, &message.text);
            queued.apply_source(message);
            assert!(queued.kind == entry.kind);
            assert_eq!(queued.source_label, entry.source_label);
        }
        let legacy: Delivery = serde_json::from_value(json!({"id":"legacy", "thread":"controller", "text":"任务反馈：旧消息", "status":"sent", "turn":null, "error":null})).unwrap();
        assert!(legacy.source.is_none());
    }
    #[test]
    fn connecting_screen_only_shows_centered_text_and_empty_chat_has_no_welcome() {
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(32, 12), (65, 22), (120, 32)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut draft = Draft::default();
            terminal
                .draw(|frame| {
                    draw(frame, &View::default(), &mut draft, true);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert_eq!(text.split_whitespace().collect::<String>(), "连接中…");
            let row = (0..width)
                .map(|x| buffer[(x, height / 2)].symbol())
                .collect::<String>();
            assert_eq!(row.split_whitespace().collect::<String>(), "连接中…");
        }
        assert!(
            history(
                &View {
                    connected: true,
                    ..View::default()
                },
                &Draft::default(),
                80
            )
            .lines
            .is_empty()
        );
    }

    #[test]
    fn narrow_and_wide_layouts_keep_input_tail_and_errors_visible() {
        use ratatui::{Terminal, backend::TestBackend};
        for (width, height) in [(32, 12), (65, 22), (120, 32)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let view = View {
                entries: vec![Entry::new(Kind::Assistant, "history\n".repeat(60))],
                status: "空闲".into(),
                connected: true,
                error: Some("SEND_ERROR".into()),
                ..View::default()
            };
            let text = format!("{}\nVISIBLE_TAIL", "中文长输入".repeat(100));
            let mut draft = Draft {
                cursor: text.len(),
                text,
                ..Draft::default()
            };
            let mut input = Rect::default();
            let completed = terminal
                .draw(|frame| input = draw(frame, &view, &mut draft, true).input)
                .unwrap();
            let text = completed
                .buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            let warning: String = (0..width)
                .map(|x| completed.buffer[(x, crate::tabs::HEADER_HEIGHT)].symbol())
                .collect();
            assert!(warning.contains("YOLO"));
            let warning_x = (0..width)
                .find(|&x| completed.buffer[(x, crate::tabs::HEADER_HEIGHT)].symbol() == "Y")
                .unwrap();
            assert_eq!(
                completed.buffer[(warning_x, crate::tabs::HEADER_HEIGHT)].fg,
                Color::Red
            );
            assert!(text.contains("VISIBLE_TAIL"), "{width}×{height}");
            assert!(text.contains("SEND_ERROR"), "{width}×{height}");
            let cursor = terminal.get_cursor_position().unwrap();
            assert!(input.contains(cursor), "{width}×{height}: {cursor:?}");
        }
    }

    #[test]
    fn input_wrap_keeps_unicode_cursor_and_newlines_visible() {
        let (lines, cursor) = input_lines("中文ab\n下一行", "中文ab\n下".len(), 4);
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["中文", "ab", "下一", "行"]
        );
        assert_eq!(cursor, (2, 2));
        let (lines, cursor) = input_lines("中文\nnext", "中文\n".len(), 4);
        assert_eq!(
            lines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["中文", "next", ""]
        );
        assert_eq!(cursor, (0, 1));
    }
}
