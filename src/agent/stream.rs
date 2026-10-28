//! 接入后产生的主控消息事件；不读取既有会话，不从历史重建事件。
use super::{Binding, MessageSource, State, Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Role {
    User,
    Codex,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Event {
    pub sequence: u64,
    pub origin: u64,
    pub role: Role,
    pub text: String,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Pending {
    thread: String,
    origin: u64,
}
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Stream {
    position: u64,
    events: Vec<Event>,
    pending: BTreeMap<String, Pending>,
    completed: BTreeSet<String>,
}
impl Stream {
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn after(&self, cursor: u64) -> Result<&[Event]> {
        ensure!(
            cursor <= self.position,
            "消息事件进度超出日志范围；请核对记录，不能重置后重播"
        );
        Ok(&self.events[self
            .events
            .partition_point(|event| event.sequence <= cursor)..])
    }
    pub fn accepted(
        &mut self,
        binding: &Binding,
        id: &str,
        text: &str,
        source: Option<&MessageSource>,
    ) {
        if binding.project.is_some() {
            return;
        }
        self.position += 1;
        self.pending.insert(
            id.into(),
            Pending {
                thread: binding.thread.clone(),
                origin: self.position,
            },
        );
        if !matches!(
            source,
            Some(MessageSource::Task { .. } | MessageSource::Completion { .. })
        ) {
            self.events.push(Event {
                sequence: self.position,
                origin: self.position,
                role: Role::User,
                text: text.into(),
            });
        }
    }
}
impl Store {
    /// 与入队共用状态锁；订阅只取得日志末端，不枚举或读取旧会话。
    pub(crate) fn subscribe_messages(&self) -> Result<u64> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        if state.stream.is_none() {
            state.stream = Some(Stream::default());
            self.save(&state)?;
        }
        Ok(state.stream.as_ref().unwrap().position())
    }
    pub(crate) fn collect_messages(&self) -> Result<()> {
        let observed = self.load()?;
        let Some(stream) = &observed.stream else {
            return Ok(());
        };
        if stream.pending.is_empty() {
            return Ok(());
        }
        let mut client = None;
        let mut errors = Vec::new();
        for (id, pending) in &stream.pending {
            let result = (|| -> Result<()> {
                let message = observed
                    .messages
                    .iter()
                    .find(|m| &m.id == id)
                    .context("新消息事件缺少对应投递记录")?;
                let text = match message.status.as_str() {
                    "failed" | "cancelled" => Some(format!(
                        "请求{}：{}",
                        message.status,
                        message.error.as_deref().unwrap_or("请检查 Twig 状态")
                    )),
                    "sent" => {
                        let turn = message.turn.as_deref().context("已投递消息缺少轮次 ID")?;
                        if client.is_none() {
                            client = Some(self.client(&observed)?);
                        }
                        let client = client.as_mut().unwrap();
                        match client.turn_status(&pending.thread, turn)?.as_str() {
                            "completed" => Some(client.final_reply(&pending.thread, turn)?),
                            "failed" | "interrupted" => {
                                Some("本轮失败或被中断，请检查 Twig Agent。".into())
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                let Some(text) = text else {
                    return Ok(());
                };
                self.finish_message(&observed, id, pending, text)
            })();
            if let Err(error) = result {
                errors.push(format!("消息 {id}：{error:#}"));
            }
        }
        ensure!(errors.is_empty(), "{}", errors.join("\n"));
        Ok(())
    }
    fn finish_message(
        &self,
        observed: &State,
        id: &str,
        pending: &Pending,
        text: String,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        ensure!(
            state.socket == observed.socket,
            "Codex 服务身份已变化，保留待收集的新消息"
        );
        let stream = state.stream.as_mut().context("消息事件日志不存在")?;
        if stream.pending.get(id) != Some(pending) {
            return Ok(());
        }
        let message = observed
            .messages
            .iter()
            .find(|m| m.id == id)
            .context("事件对应的投递记录丢失")?;
        let key =
            serde_json::json!([pending.thread, message.turn.as_deref().unwrap_or(id)]).to_string();
        if stream.completed.insert(key) && !text.is_empty() {
            stream.position += 1;
            stream.events.push(Event {
                sequence: stream.position,
                origin: pending.origin,
                role: Role::Codex,
                text,
            });
        }
        stream.pending.remove(id);
        self.save(&state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Delivery;

    #[test]
    fn completion_callback_tracks_controller_reply_without_publishing_child_text() {
        let mut state = State {
            stream: Some(Stream::default()),
            ..State::default()
        };
        let controller = Binding {
            thread: "controller".into(),
            cwd: "/controller".into(),
            project: None,
            task: None,
            stopped: false,
        };
        let task = Binding {
            thread: "child".into(),
            cwd: "/child".into(),
            project: Some("demo".into()),
            task: Some("task".into()),
            stopped: false,
        };
        state.bindings.insert("controller".into(), controller);
        crate::agent::record_completion(&mut state, &task, "completed-turn").unwrap();
        crate::agent::record_completion(&mut state, &task, "completed-turn").unwrap();
        let stream = state.stream.as_ref().unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(stream.position(), 1);
        assert!(stream.after(0).unwrap().is_empty());
        assert_eq!(stream.pending[&state.messages[0].id].thread, "controller");
    }

    #[test]
    fn enabling_stream_does_not_import_old_messages_and_final_events_are_durable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(&root.path().join("config.toml")).unwrap();
        let binding = Binding {
            thread: "controller".into(),
            cwd: root.path().into(),
            project: None,
            task: None,
            stopped: false,
        };
        let delivery = |id: &str| Delivery {
            id: id.into(),
            thread: "controller".into(),
            text: id.into(),
            source: None,
            status: "sent".into(),
            turn: Some("new-turn".into()),
            error: None,
        };
        let mut state = State::default();
        state.messages.push(delivery("old-message"));
        {
            let _lock = store.lock().unwrap();
            store.save(&state).unwrap();
        }
        assert_eq!(store.subscribe_messages().unwrap(), 0);
        let mut state = store.load().unwrap();
        assert!(state.stream.as_ref().unwrap().pending.is_empty());
        for (id, source) in [
            ("new-user", None),
            (
                "child-report",
                Some(MessageSource::Task {
                    project: "p".into(),
                    task: "t".into(),
                }),
            ),
        ] {
            state.messages.push(delivery(id));
            state
                .stream
                .as_mut()
                .unwrap()
                .accepted(&binding, id, id, source.as_ref());
        }
        assert_eq!(state.stream.as_ref().unwrap().position(), 2);
        {
            let _lock = store.lock().unwrap();
            store.save(&state).unwrap();
        }
        let observed = store.load().unwrap();
        let pending = observed.stream.as_ref().unwrap().pending["new-user"].clone();
        store
            .finish_message(&observed, "new-user", &pending, "final reply".into())
            .unwrap();
        // 两条输入被 Codex 合并到同一轮时，只发布一份最终回复；并发重试同样去重。
        store
            .finish_message(&observed, "new-user", &pending, "final reply".into())
            .unwrap();
        store
            .finish_message(
                &observed,
                "child-report",
                &observed.stream.as_ref().unwrap().pending["child-report"],
                "final reply".into(),
            )
            .unwrap();
        let restored = store.load().unwrap();
        let stream = restored.stream.unwrap();
        assert_eq!(stream.position(), 3);
        assert!(stream.pending.is_empty());
        assert_eq!(stream.after(0).unwrap()[0].text, "new-user");
        assert_eq!(stream.after(1).unwrap()[0].text, "final reply");
        assert_eq!(store.subscribe_messages().unwrap(), 3);
    }
}
