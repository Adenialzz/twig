//! 原生 Chat；后台线程只负责本客户端通信，退出不停止 Codex。
use super::{Binding, Store, rpc};
mod view;
use crate::console::selection::Selection;
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use serde_json::{Value, json};
use std::{io::Stdout, path::PathBuf, sync::mpsc, thread, time::Duration};
use view::{Entry, Kind};

enum Command {
    Send(String),
    Answer(bool),
    Refresh,
    Exit,
}
#[derive(Default)]
struct View {
    entries: Vec<Entry>,
    thread: String,
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
    history_height: u16,
    details: bool,
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
    let mut connection: Option<(Store, Binding, rpc::Client, bool)> = None;
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
                let subscribed = store.resume(&mut client, &binding)?;
                store.drain()?;
                Ok((store, binding, client, subscribed))
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
        let mut reconnect = false;
        if let Some((store, binding, client, subscribed)) = &mut connection {
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
                if !*subscribed {
                    *subscribed = store.resume(client, binding)?;
                }
                let mut result = client.thread(&binding.thread)?;
                if result["thread"]["status"]["type"] == "notLoaded" {
                    *subscribed = store.resume(client, binding)?;
                    result = client.thread(&binding.thread)?;
                }
                view.entries = view::transcript(&result);
                view.thread = binding.thread.clone();
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
                view.status = crate::codex::status_label(&result["thread"]["status"]);
                if pending > 0 {
                    view.status.push_str(&format!(" · 排队 {pending}"));
                }
                if unknown > 0 {
                    view.status.push_str(&format!(" · 待核对 {unknown}"));
                }
                for message in state
                    .messages
                    .iter()
                    .filter(|m| matches!(m.status.as_str(), "queued" | "sending" | "failed"))
                {
                    let label = match message.status.as_str() {
                        "queued" => "等待投递",
                        "sending" => "投递结果待核对",
                        _ => "投递失败",
                    };
                    let mut entry =
                        Entry::new(Kind::Workflow, format!("{label}\n{}", message.text));
                    entry.detail = format!("消息 ID：{}", message.id);
                    view.entries.push(entry);
                    if let Some(error) = &message.error {
                        view.entries.push(Entry::new(Kind::Error, error));
                    }
                }
                if pending > 0 && result["thread"]["status"]["type"] == "idle" {
                    store.flush()?;
                }
                if let Some(request) = client.requests.first() {
                    view.approval = Some(format!("{}\n{}", request["method"], request["params"]));
                }
                Ok(())
            })();
            if let Err(error) = result {
                reconnect = rpc::thread_not_loaded(&error, &binding.thread)
                    || error.downcast_ref::<tungstenite::Error>().is_some();
                if reconnect {
                    view.connected = false;
                    view.status = "连接失效，正在重新检查主控会话…".into();
                }
                view.error = Some(format!("{error:#}"));
            }
        } else {
            if let Some(Command::Send(text)) = command {
                view.rejected = Some(text);
            }
            view.error = last_error.clone();
            view.status = "未连接 · F5 重试".into();
        }
        if reconnect {
            connection = None;
            last_error = None;
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
    let mut dirty = true;
    let mut selection: Option<Selection> = None;
    let mut select_at = None;
    let mut areas = view::Areas::default();
    let mut copy_notice = None;
    loop {
        while let Ok(mut update) = views.try_recv() {
            if let Some(rejected) = update.rejected.take() {
                draft.text = if draft.text.is_empty() {
                    rejected
                } else {
                    format!("{rejected}\n{}", draft.text)
                };
                draft.cursor = draft.text.len();
            }
            if update.error.is_some() && update.entries.is_empty() {
                update.entries = std::mem::take(&mut view.entries);
            }
            view = update;
            dirty = true;
        }
        if let Some(notice) = twig_cli::clipboard::poll() {
            copy_notice = Some(notice);
            dirty = true;
        }
        if copy_notice
            .as_ref()
            .is_some_and(twig_cli::clipboard::Notice::expired)
        {
            copy_notice = None;
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| {
                if selection
                    .as_ref()
                    .is_some_and(|selected| !selected.fits(frame.area()))
                {
                    selection = None;
                }
                areas = view::draw(frame, &view, draft, selection.is_none());
                if let Some((x, y)) = select_at.take() {
                    selection = [areas.history, areas.input]
                        .into_iter()
                        .find(|area| area.contains((x, y).into()))
                        .map(|area| Selection::new(frame.buffer_mut(), area, x, y));
                }
                if let Some(selected) = &selection {
                    selected.draw(frame.buffer_mut());
                }
                if let Some(notice) = &copy_notice {
                    notice.draw(frame);
                }
            })?;
            dirty = false;
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
            Event::Resize(_, _) => {
                selection = None;
                draft.history_height = 0;
            }
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    selection = None;
                    select_at = Some((mouse.column, mouse.row));
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(selected) = &mut selection
                        && selected.dragging
                    {
                        selected.update(mouse.column, mouse.row);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(selected) = &mut selection
                        && selected.dragging
                    {
                        selected.update(mouse.column, mouse.row);
                        selected.dragging = false;
                        let text = selected.text();
                        if text.is_empty() {
                            selection = None;
                        } else {
                            copy_notice = Some(twig_cli::clipboard::start(text));
                        }
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if areas.history.contains((mouse.column, mouse.row).into()) =>
                {
                    selection = None;
                    draft.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                        draft.scroll.saturating_add(3)
                    } else {
                        draft.scroll.saturating_sub(3)
                    };
                }
                _ => {}
            },
            Event::Paste(text) => {
                selection = None;
                let text: String = text
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\t', "    ")
                    .chars()
                    .filter(|c| !c.is_control() || *c == '\n')
                    .collect();
                draft.text.insert_str(draft.cursor, &text);
                draft.cursor += text.len();
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if selection.take().is_some() && key.code == KeyCode::Esc {
                    continue;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::F(2) => return Ok(false),
                    KeyCode::F(5) => tx.send(Command::Refresh)?,
                    KeyCode::F(4) => {
                        draft.details = !draft.details;
                        draft.history_height = 0;
                    }
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(true);
                    }
                    KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tx.send(Command::Answer(true))?
                    }
                    KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        tx.send(Command::Answer(false))?
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                        if let Some(entry) = view
                            .entries
                            .iter()
                            .rev()
                            .find(|entry| entry.kind == Kind::Assistant)
                        {
                            copy_notice = Some(twig_cli::clipboard::start(entry.text.clone()));
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.text.clear();
                        draft.cursor = 0;
                    }
                    KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                        draft.text.insert(draft.cursor, '\n');
                        draft.cursor += 1;
                    }
                    KeyCode::Enter if !draft.text.trim().is_empty() && view.connected => {
                        view.status = "正在保存并发送…".into();
                        tx.send(Command::Send(std::mem::take(&mut draft.text)))?;
                        draft.cursor = 0;
                        draft.scroll = 0;
                    }
                    KeyCode::Enter if !draft.text.trim().is_empty() => {
                        view.error = Some(
                            "连接尚未就绪，输入已保留；连接成功后再按 Enter，或 F5 重试。".into(),
                        );
                    }
                    KeyCode::Left => {
                        draft.cursor = draft.text[..draft.cursor]
                            .char_indices()
                            .last()
                            .map_or(0, |(i, _)| i)
                    }
                    KeyCode::Right => {
                        if let Some(c) = draft.text[draft.cursor..].chars().next() {
                            draft.cursor += c.len_utf8();
                        }
                    }
                    KeyCode::Home => draft.cursor = 0,
                    KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.scroll = 0
                    }
                    KeyCode::End => draft.cursor = draft.text.len(),
                    KeyCode::Backspace if draft.cursor > 0 => {
                        let previous = draft.text[..draft.cursor].char_indices().last().unwrap().0;
                        draft.text.drain(previous..draft.cursor);
                        draft.cursor = previous;
                    }
                    KeyCode::Delete if draft.cursor < draft.text.len() => {
                        let next = draft.cursor
                            + draft.text[draft.cursor..]
                                .chars()
                                .next()
                                .unwrap()
                                .len_utf8();
                        draft.text.drain(draft.cursor..next);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        draft.text.insert(draft.cursor, c);
                        draft.cursor += c.len_utf8();
                    }
                    KeyCode::PageUp => draft.scroll = draft.scroll.saturating_add(10),
                    KeyCode::PageDown => draft.scroll = draft.scroll.saturating_sub(10),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
