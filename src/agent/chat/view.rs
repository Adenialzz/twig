use super::{Draft, View};
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
    Workflow,
    Tool,
    Error,
}

pub(super) struct Entry {
    pub id: Option<String>,
    pub kind: Kind,
    pub text: String,
    pub detail: String,
}
impl Entry {
    pub fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            text: clean(&text.into()),
            detail: String::new(),
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
                            let kind = if text.starts_with("任务反馈：") {
                                Kind::Workflow
                            } else {
                                Kind::User
                            };
                            let mut entry = Entry::new(kind, text);
                            entry.id = item["clientId"].as_str().map(str::to_owned);
                            entry
                        }
                        Some("agentMessage") => {
                            Entry::new(Kind::Assistant, item["text"].as_str().unwrap_or_default())
                        }
                        Some("commandExecution") => {
                            let status = match item["status"].as_str() {
                                Some("completed") => "完成",
                                Some("inProgress") => "执行中",
                                Some("failed") => "失败",
                                Some("declined") => "已拒绝",
                                _ => "待确认",
                            };
                            let mut entry = Entry::new(
                                Kind::Tool,
                                format!(
                                    "{status} · {}",
                                    item["command"].as_str().unwrap_or_default()
                                ),
                            );
                            entry.detail =
                                clean(item["aggregatedOutput"].as_str().unwrap_or_default());
                            entries.push(entry);
                            if item["status"] == "failed" {
                                entries.push(Entry::new(
                                    Kind::Error,
                                    item["aggregatedOutput"].as_str().unwrap_or("命令执行失败"),
                                ));
                            }
                            continue;
                        }
                        Some("fileChange") => {
                            let paths = item["changes"]
                                .as_array()
                                .map(|changes| {
                                    changes
                                        .iter()
                                        .filter_map(|c| c["path"].as_str())
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            Entry::new(Kind::Tool, format!("文件修改\n{paths}"))
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

fn markdown(text: &str) -> Vec<Line<'static>> {
    let mut code = false;
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                code = !code;
                return Line::styled(
                    if code {
                        format!("  代码 · {}", line.trim_start().trim_start_matches('`'))
                    } else {
                        "  ─────".into()
                    },
                    Style::default().fg(Color::DarkGray),
                );
            }
            if code {
                Line::styled(
                    format!("  {line}"),
                    Style::default().fg(Color::White).bg(Color::Indexed(235)),
                )
            } else if line.starts_with('#') {
                Line::styled(
                    line.trim_start_matches('#').trim_start().to_owned(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(line.to_owned())
            }
        })
        .collect()
}

fn history(view: &View, draft: &Draft) -> Text<'static> {
    let mut lines = Vec::new();
    for entry in &view.entries {
        let (label, color) = match entry.kind {
            Kind::User => ("你", Color::Cyan),
            Kind::Assistant => ("主控", Color::Green),
            Kind::Workflow => ("任务反馈", Color::Yellow),
            Kind::Tool => ("工具", Color::DarkGray),
            Kind::Error => ("需要处理", Color::Red),
        };
        lines.push(Line::styled(
            format!("▎ {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
        if entry.kind == Kind::Tool && !draft.details {
            let summary = entry.text.lines().next().unwrap_or_default();
            let shortened: String = summary.chars().take(90).collect();
            lines.push(Line::styled(
                format!(
                    "{shortened}{}",
                    if summary.chars().count() > 90 {
                        "…"
                    } else {
                        ""
                    }
                ),
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            lines.extend(markdown(&entry.text));
            if draft.details && !entry.detail.is_empty() {
                lines.extend(markdown(&format!("```\n{}\n```", entry.detail)));
            }
        }
        lines.push(Line::from(""));
    }
    for (label, text, color) in [
        ("连接 / 投递错误", &view.error, Color::Red),
        (
            "等待审批 · Ctrl-Y 同意 / Ctrl-N 拒绝",
            &view.approval,
            Color::Yellow,
        ),
    ] {
        if let Some(text) = text {
            lines.push(Line::styled(
                label,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown(&clean(text)));
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
        screen.y + crate::tabs::HEADER_HEIGHT,
        width,
        screen.height - crate::tabs::HEADER_HEIGHT,
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
        Constraint::Length(2),
    ])
    .split(body);
    crate::tabs::draw(
        frame,
        screen,
        crate::tabs::Tab::Agent,
        "Tab 切换 · F5 重连 · Ctrl-C 清空/退出",
        false,
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
    let transcript = Paragraph::new(history(view, draft)).wrap(Wrap { trim: false });
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
            "Enter 发送 · Alt-Enter 换行 · Ctrl-U 清空\nAlt-C 复制回复 · F4 {}工具详情",
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
                &Draft::default()
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
