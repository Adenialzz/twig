//! 原生 Chat；后台线程只负责本客户端通信，退出不停止 Codex。
use super::{Binding, Store, rpc};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use serde_json::{Value, json};
use std::{
    io::Stdout,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

enum Command {
    Send(String),
    Answer(bool),
    Refresh,
    Exit,
}
#[derive(Default)]
struct View {
    text: String,
    status: String,
    error: Option<String>,
    approval: Option<String>,
    connected: bool,
    rejected: Option<String>,
}

#[derive(Default)]
pub struct Draft {
    text: String,
    cursor: usize,
    scroll: u16,
}

fn transcript(value: &Value) -> String {
    let mut text = String::new();
    if let Some(turns) = value["thread"]["turns"].as_array() {
        for turn in turns {
            if let Some(items) = turn["items"].as_array() {
                for item in items {
                    match item["type"].as_str() {
                        Some("userMessage") => {
                            text.push_str("你 / 工作流消息\n");
                            if let Some(content) = item["content"].as_array() {
                                for part in content {
                                    if let Some(s) = part["text"].as_str() {
                                        text.push_str(s);
                                        text.push('\n');
                                    }
                                }
                            }
                        }
                        Some("agentMessage") => {
                            text.push_str("主控\n");
                            text.push_str(item["text"].as_str().unwrap_or_default());
                            text.push('\n');
                        }
                        Some("commandExecution") => {
                            text.push_str(&format!(
                                "工具 [{}] {}\n",
                                item["status"].as_str().unwrap_or(""),
                                item["command"].as_str().unwrap_or("")
                            ));
                        }
                        Some("fileChange") => text.push_str("工具：文件修改\n"),
                        _ => {}
                    }
                }
            }
            if !turn["error"].is_null() {
                text.push_str(&format!("轮次错误：{}\n", turn["error"]));
            }
            text.push('\n');
        }
    }
    if text.is_empty() {
        text = "向主控交代目标，例如：查看所有项目，给 demo 创建一个修复登录问题的任务。\n".into();
    }
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn answer(request: &Value, accept: bool) -> Result<Value> {
    use anyhow::bail;
    match request["method"].as_str() {
        Some("item/commandExecution/requestApproval" | "item/fileChange/requestApproval") => {
            Ok(json!({"decision":if accept {"accept"} else {"decline"}}))
        }
        Some("item/permissions/requestApproval") => Ok(
            json!({"permissions":if accept {request["params"]["permissions"].clone()} else {json!({})},"scope":"turn"}),
        ),
        Some("mcpServer/elicitation/request") if !accept => {
            Ok(json!({"action":"decline","content":null}))
        }
        _ => bail!(
            "此输入请求需在原生 Codex 中处理：codex resume <主控 thread ID>；Chat 可继续查看状态"
        ),
    }
}

fn worker(registry: PathBuf, input: mpsc::Receiver<Command>, output: mpsc::Sender<View>) {
    let mut connection: Option<(Store, Binding, rpc::Client)> = None;
    let mut last_error = None;
    let mut refresh = true;
    loop {
        let command = if refresh {
            None
        } else {
            match input.recv_timeout(Duration::from_millis(700)) {
                Ok(Command::Exit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            }
        };
        refresh = false;
        if matches!(command, Some(Command::Refresh)) {
            connection = None;
            last_error = None;
        }
        // 连接失败后仅显式刷新重试，不每秒反复启动外部查询。
        if connection.is_none()
            && (last_error.is_none() || matches!(command, Some(Command::Refresh)))
        {
            let result = (|| -> Result<_> {
                let store = Store::new(&registry)?;
                let binding = store.initialize()?;
                let state = store.load()?;
                let mut client = store.client(&state)?;
                store.resume(&mut client, &binding)?;
                store.drain()?;
                Ok((store, binding, client))
            })();
            match result {
                Ok(c) => {
                    connection = Some(c);
                    last_error = None;
                }
                Err(e) => last_error = Some(format!("{e:#}")),
            }
        }
        let mut view = View::default();
        if let Some((store, binding, client)) = &mut connection {
            view.connected = true;
            let result = (|| -> Result<()> {
                match command {
                    Some(Command::Send(text)) => {
                        if let Err(error) = store.enqueue(binding, text.clone()) {
                            view.rejected = Some(text);
                            return Err(error);
                        }
                    }
                    Some(Command::Answer(accept)) => {
                        if let Some(request) = client.requests.first().cloned() {
                            client.answer(&request, answer(&request, accept)?)?;
                            client.requests.remove(0);
                        }
                    }
                    _ => {}
                }
                let result = client.thread(&binding.thread)?;
                view.text = transcript(&result);
                let state = store.load()?;
                let pending = state
                    .messages
                    .iter()
                    .filter(|m| m.status == "queued")
                    .count();
                let unknown = state
                    .messages
                    .iter()
                    .filter(|m| m.status == "sending")
                    .count();
                view.status = format!(
                    "{} · {} · 排队 {} · 待核对 {}",
                    crate::codex::status_label(&result["thread"]["status"]),
                    binding.thread,
                    pending,
                    unknown
                );
                for message in state
                    .messages
                    .iter()
                    .filter(|m| matches!(m.status.as_str(), "queued" | "sending" | "failed"))
                {
                    view.text.push_str(&format!(
                        "\n投递 {} [{}] {}\n{}\n",
                        message.id,
                        message.status,
                        message.text,
                        message.error.as_deref().unwrap_or("")
                    ));
                }
                if pending > 0 && result["thread"]["status"]["type"] == "idle" {
                    store.flush()?;
                }
                if let Some(request) = client.requests.first() {
                    view.approval = Some(format!(
                        "{}\n{}\nCtrl-Y 同意 · Ctrl-N 拒绝（复杂输入请用原生 Codex）",
                        request["method"], request["params"]
                    ));
                }
                Ok(())
            })();
            if let Err(error) = result {
                view.error = Some(format!("{error:#}"));
            }
        } else {
            view.error = last_error.clone();
            view.status = "未连接 · F5 重试".into();
        }
        if output.send(view).is_err() {
            break;
        }
    }
}

/// true 表示退出整个 UI；false 返回原 Console，保留导航选择。
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    registry: PathBuf,
    draft: &mut Draft,
) -> Result<bool> {
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let (tx, rx) = mpsc::channel();
    let (updates, views) = mpsc::channel();
    thread::spawn(move || worker(registry, rx, updates));
    let result = run_ui(terminal, &tx, views, draft);
    let _ = tx.send(Command::Exit);
    execute!(std::io::stdout(), DisableBracketedPaste)?;
    terminal.clear()?;
    result
}

fn run_ui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    tx: &mpsc::Sender<Command>,
    views: mpsc::Receiver<View>,
    draft: &mut Draft,
) -> Result<bool> {
    let mut view = View {
        status: "正在连接 Codex…".into(),
        ..View::default()
    };
    let mut input = std::mem::take(&mut draft.text);
    let mut cursor = draft.cursor;
    let mut scroll = draft.scroll;
    let mut dirty = true;
    let mut last = Instant::now();
    loop {
        while let Ok(mut update) = views.try_recv() {
            if let Some(rejected) = update.rejected.take() {
                input = if input.is_empty() {
                    rejected
                } else {
                    format!("{rejected}\n{input}")
                };
                cursor = input.len();
            }
            view = update;
            dirty = true;
        }
        if dirty || last.elapsed() > Duration::from_secs(1) {
            terminal.draw(|frame| {
                let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(3), Constraint::Length(5), Constraint::Length(2)]).split(frame.area());
                crate::tabs::draw(frame, rows[0], crate::tabs::Tab::Chat, "Tab 切换标签 · Ctrl-Q 退出 · F5 重连");
                frame.render_widget(Paragraph::new(view.status.as_str()).style(Style::default().fg(Color::Cyan)), ratatui::layout::Rect::new(rows[0].x, rows[0].y + 1, rows[0].width, rows[0].height.saturating_sub(1)));
                let mut content = view.text.clone();
                if let Some(error) = &view.error { content.push_str(&format!("\n错误：{error}\n")); }
                if let Some(approval) = &view.approval { content.push_str(&format!("\n等待审批：{approval}\n")); }
                let paragraph = Paragraph::new(content).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" 主控与任务进展 · PgUp/PgDn 滚动 "));
                let height = paragraph.line_count(rows[1].width.saturating_sub(2)).min(u16::MAX as usize) as u16;
                let bottom = height.saturating_sub(rows[1].height.saturating_sub(2));
                frame.render_widget(paragraph.scroll((bottom.saturating_sub(scroll), 0)), rows[1]);
                let (before, after) = input.split_at(cursor);
                frame.render_widget(Paragraph::new(format!("{before}▏{after}")).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(" 输入 · Enter 发送 · Alt-Enter 换行 ")), rows[2]);
                frame.render_widget(Paragraph::new("退出不会停止 Codex；daemon 不可用时请自行启动。\nCtrl-Y / Ctrl-N 审批 · Ctrl-U 清空输入"), rows[3]);
            })?;
            dirty = false;
            last = Instant::now();
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        dirty = true;
        let event = match event::read()? {
            Event::Mouse(mouse)
                if crate::tabs::clicked(mouse, terminal.get_frame().area())
                    == Some(crate::tabs::Tab::Console) =>
            {
                Event::Key(event::KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
            }
            event => event,
        };
        match event {
            Event::Paste(text) => {
                input.insert_str(cursor, &text);
                cursor += text.len();
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Tab | KeyCode::BackTab | KeyCode::F(2) => {
                    *draft = Draft {
                        text: input,
                        cursor,
                        scroll,
                    };
                    return Ok(false);
                }
                KeyCode::F(5) => {
                    tx.send(Command::Refresh)?;
                }
                KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(true);
                }
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    tx.send(Command::Answer(true))?;
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    tx.send(Command::Answer(false))?;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear();
                    cursor = 0;
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    input.insert(cursor, '\n');
                    cursor += 1;
                }
                KeyCode::Enter if !input.trim().is_empty() && view.connected => {
                    tx.send(Command::Send(std::mem::take(&mut input)))?;
                    cursor = 0;
                    scroll = 0;
                }
                KeyCode::Enter if !input.trim().is_empty() => {
                    view.error =
                        Some("连接尚未就绪，输入已保留；连接成功后再按 Enter，或 F5 重试。".into());
                }
                KeyCode::Left => {
                    cursor = input[..cursor].char_indices().last().map_or(0, |(i, _)| i)
                }
                KeyCode::Right => {
                    if let Some(c) = input[cursor..].chars().next() {
                        cursor += c.len_utf8();
                    }
                }
                KeyCode::Home => cursor = 0,
                KeyCode::End => cursor = input.len(),
                KeyCode::Backspace if cursor > 0 => {
                    let previous = input[..cursor].char_indices().last().unwrap().0;
                    input.drain(previous..cursor);
                    cursor = previous;
                }
                KeyCode::Delete if cursor < input.len() => {
                    let next = cursor + input[cursor..].chars().next().unwrap().len_utf8();
                    input.drain(cursor..next);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.insert(cursor, c);
                    cursor += c.len_utf8();
                }
                KeyCode::PageUp => scroll = scroll.saturating_add(10),
                KeyCode::PageDown => scroll = scroll.saturating_sub(10),
                _ => {}
            },
            _ => {}
        }
    }
}
