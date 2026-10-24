//! 原生 Chat；后台线程只负责本客户端通信，退出不停止 Codex。
use super::{Binding, MessageSource, Store, rpc};
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

use std::{
    collections::HashSet,
    io::Stdout,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
use view::{Entry, Kind};

enum Command {
    Send(String, String),
    Refresh,
    Viewing(bool),
    Exit,
}
#[derive(Default)]
struct View {
    entries: Vec<Entry>,
    thread: String,
    status: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    error: Option<String>,
    approval: Option<String>,
    connected: bool,
    rejected: Option<(String, String)>,
}

#[derive(Default)]
pub struct Draft {
    text: String,
    cursor: usize,
    scroll: u16,
    history_height: u16,
    details: bool,
    pending: Vec<(String, String)>,
    runtime: Option<Runtime>,
}

impl Draft {
    fn merge_pending(&mut self, view: &mut View) {
        let mut ids: HashSet<String> = view
            .entries
            .iter()
            .filter_map(|entry| entry.id.clone())
            .collect();
        for (id, text) in &self.pending {
            if !ids.insert(id.clone()) {
                continue;
            }
            let mut entry = Entry::new(Kind::User, text);
            entry.id = Some(id.clone());
            view.entries.push(entry);
        }
    }
}

#[derive(Default)]
struct Updates {
    latest: Option<View>,
    rejected: Vec<(String, String)>,
}

struct Runtime {
    tx: mpsc::Sender<Command>,
    updates: Arc<Mutex<Updates>>,
    view: View,
    handle: Option<thread::JoinHandle<()>>,
}
impl Runtime {
    fn start(registry: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let updates = Arc::new(Mutex::new(Updates::default()));
        let output = updates.clone();
        let handle = thread::spawn(move || worker(registry, rx, output));
        Self {
            tx,
            updates,
            view: View::default(),
            handle: Some(handle),
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        // Exit 排在已接受的发送命令后，退出前完成持久化，不因切页丢弃输入。
        let _ = self.tx.send(Command::Exit);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker(registry: PathBuf, input: mpsc::Receiver<Command>, output: Arc<Mutex<Updates>>) {
    let mut connection: Option<(Store, Binding, rpc::Client, bool)> = None;
    let mut last_error = None;
    let mut refresh = true;
    let mut viewing = true;
    loop {
        let command = if refresh {
            None
        } else {
            let received = if viewing {
                input.recv_timeout(Duration::from_millis(700))
            } else {
                input
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            match received {
                Ok(Command::Exit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            }
        };
        refresh = false;
        if let Some(Command::Viewing(active)) = command {
            viewing = active;
            if !viewing {
                continue;
            }
        }
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
                Ok((store, binding, client, subscribed))
            })();
            match result {
                Ok(c) => {
                    last_error = c.0.drain().err().map(|e| format!("队列补查失败：{e:#}"));
                    connection = Some(c);
                }
                Err(e) => last_error = Some(format!("{e:#}")),
            }
        }
        let mut view = View {
            error: last_error.clone(),
            ..View::default()
        };
        let mut reconnect = false;
        if let Some((store, binding, client, subscribed)) = &mut connection {
            view.connected = true;
            let result = (|| -> Result<()> {
                if let Some(Command::Send(id, text)) = command
                    && let Err(error) = store.enqueue_with_id(
                        binding,
                        text.clone(),
                        id.clone(),
                        Some(MessageSource::User),
                    )
                {
                    view.rejected = Some((id, text));
                    last_error = Some(format!("{error:#}"));
                    return Err(error);
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
                view.model = result["thread"]["model"].as_str().map(str::to_owned);
                view.reasoning_effort = result["thread"]["reasoningEffort"]
                    .as_str()
                    .map(str::to_owned);
                let state = store.load()?;
                view::apply_sources(&mut view.entries, &state.messages, &binding.thread);
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
                let mut ids: HashSet<String> = view
                    .entries
                    .iter()
                    .filter_map(|entry| entry.id.clone())
                    .collect();
                for message in state.messages.iter().filter(|m| {
                    m.thread == binding.thread
                        && matches!(m.status.as_str(), "queued" | "sending" | "sent" | "failed")
                }) {
                    if !ids.insert(message.id.clone()) {
                        continue;
                    }
                    let mut entry = Entry::new(Kind::Unknown, &message.text);
                    entry.apply_source(message);
                    entry.id = Some(message.id.clone());
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
                    view.approval = Some(format!(
                        "{}\n{}\n请在原生 Codex 中处理该交互请求。",
                        request["method"], request["params"]
                    ));
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
            if let Some(Command::Send(id, text)) = command {
                view.rejected = Some((id, text));
            }
            view.error = last_error.clone();
            view.status = "未连接 · F5 重试".into();
        }
        if reconnect {
            connection = None;
            last_error = None;
        }
        let mut updates = output.lock().unwrap();
        if let Some(rejected) = view.rejected.take() {
            updates.rejected.push(rejected);
        }
        updates.latest = Some(view);
    }
}

/// true 表示退出整个 UI；false 返回原 Console，保留导航选择。
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    registry: PathBuf,
    draft: &mut Draft,
) -> Result<bool> {
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let mut runtime = draft
        .runtime
        .take()
        .unwrap_or_else(|| Runtime::start(registry));
    runtime.tx.send(Command::Viewing(true))?;
    let result = run_ui(terminal, &mut runtime, draft);
    let _ = runtime.tx.send(Command::Viewing(false));
    draft.runtime = Some(runtime);
    execute!(std::io::stdout(), DisableBracketedPaste)?;
    terminal.clear()?;
    result
}

fn run_ui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    runtime: &mut Runtime,
    draft: &mut Draft,
) -> Result<bool> {
    let Runtime {
        tx, updates, view, ..
    } = runtime;
    draft.merge_pending(view);
    let mut dirty = true;
    let mut selection: Option<Selection> = None;
    let mut select_at = None;
    let mut areas = view::Areas::default();
    let mut copy_notice = None;
    loop {
        let (update, rejected) = {
            let mut updates = updates.lock().unwrap();
            (updates.latest.take(), std::mem::take(&mut updates.rejected))
        };
        for (id, rejected) in rejected {
            draft.pending.retain(|(pending, _)| pending != &id);
            view.entries
                .retain(|entry| entry.id.as_deref() != Some(&id));
            draft.text = if draft.text.is_empty() {
                rejected
            } else {
                format!("{rejected}\n{}", draft.text)
            };
            draft.cursor = draft.text.len();
            dirty = true;
        }
        if let Some(mut update) = update {
            let ids: HashSet<_> = update
                .entries
                .iter()
                .filter_map(|entry| entry.id.as_deref())
                .collect();
            draft.pending.retain(|(id, _)| !ids.contains(id.as_str()));
            if update.error.is_some() && update.entries.is_empty() {
                update.entries = std::mem::take(&mut view.entries);
            }
            draft.merge_pending(&mut update);
            *view = update;
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
                areas = view::draw(frame, view, draft, selection.is_none());
                if !view.connected && view.error.is_none() {
                    return;
                }
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
        let event = event::read()?;
        if !view.connected && view.error.is_none() {
            match &event {
                Event::Resize(_, _) => {}
                Event::Key(key)
                    if matches!(key.code, KeyCode::Tab | KeyCode::F(5))
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)) => {}
                _ => continue,
            }
        }
        let event = match event {
            Event::Mouse(mouse)
                if crate::tabs::clicked(mouse, terminal.get_frame().area())
                    == Some(crate::tabs::Tab::Console) =>
            {
                Event::Key(event::KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
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
                    KeyCode::Tab => return Ok(false),
                    KeyCode::F(5) => {
                        tx.send(Command::Refresh)?;
                        view.connected = false;
                        view.error = None;
                        selection = None;
                    }
                    KeyCode::F(4) => {
                        draft.details = !draft.details;
                        draft.history_height = 0;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if draft.text.is_empty() {
                            return Ok(true);
                        }
                        draft.text.clear();
                        draft.cursor = 0;
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
                        let id = super::message_id();
                        let text = std::mem::take(&mut draft.text);
                        tx.send(Command::Send(id.clone(), text.clone()))?;
                        draft.pending.push((id, text));
                        draft.merge_pending(view);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_echo_survives_stale_snapshots_and_merges_by_id_not_text() {
        let mut draft = Draft {
            pending: vec![("a".into(), "same".into()), ("b".into(), "same".into())],
            ..Draft::default()
        };
        let mut view = View::default();
        draft.merge_pending(&mut view);
        draft.merge_pending(&mut view);
        assert_eq!(view.entries.len(), 2);
        assert_eq!(draft.pending.len(), 2);
        let mut fresh = View {
            entries: view::transcript(
                &json!({"thread":{"turns":[{"items":[{"type":"userMessage","clientId":"a","content":[{"text":"same"}]}]}]}}),
            ),
            ..View::default()
        };
        draft.merge_pending(&mut fresh);
        assert_eq!(fresh.entries.len(), 2);
        assert_eq!(fresh.entries[0].id.as_deref(), Some("a"));
        assert_eq!(fresh.entries[1].id.as_deref(), Some("b"));
        let mut stale = View::default();
        draft.merge_pending(&mut stale);
        assert_eq!(stale.entries.len(), 2);
    }
}
