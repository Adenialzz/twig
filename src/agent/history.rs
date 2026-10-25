use crate::agent::{Delivery, MessageSource};
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Kind {
    User,
    Assistant,
    Task,
    Completion,
    Unknown,
    Tool,
    Error,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) enum Tool {
    Command {
        status: String,
        exit_code: Option<i64>,
    },
    Files {
        status: String,
        changes: Vec<(String, String)>,
    },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Entry {
    pub id: Option<String>,
    pub key: String,
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
            key: String::new(),
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
            Some(MessageSource::Feishu { .. }) => (Kind::User, Some("飞书".into())),
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
pub(crate) fn apply_sources(entries: &mut [Entry], messages: &[Delivery], thread: &str) {
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

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// 所有前端共用同一轮次/条目顺序和来源映射；未知类型保留原始公开字段。
pub(crate) fn transcript(value: &Value) -> Vec<Entry> {
    let mut entries = Vec::new();
    if let Some(turns) = value["thread"]["turns"].as_array() {
        for (turn_index, turn) in turns.iter().enumerate() {
            let turn_key = turn["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("turn-{turn_index}"));
            if let Some(items) = turn["items"].as_array() {
                for (index, item) in items.iter().enumerate() {
                    let mut entry = match item["type"].as_str() {
                        Some("userMessage") => {
                            let text = item["content"]
                                .as_array()
                                .map(|parts| {
                                    parts
                                        .iter()
                                        .map(|part| {
                                            part["text"]
                                                .as_str()
                                                .map(str::to_owned)
                                                .unwrap_or_else(|| part.to_string())
                                        })
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
                        _ => Entry::new(Kind::Tool, item["type"].as_str().unwrap_or("未知条目")),
                    };
                    entry.key = serde_json::json!([
                        turn_key,
                        item["id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("item-{index}"))
                    ])
                    .to_string();
                    // 工具、补丁、MCP、图像等不再静默丢弃；Console 可折叠展示完整详情。
                    if entry.kind == Kind::Tool {
                        entry.detail = clean(&serde_json::to_string_pretty(item).unwrap());
                    }
                    if item["type"] == "commandExecution" && item["status"] == "failed" {
                        let mut failure = Entry::new(
                            Kind::Error,
                            item["aggregatedOutput"].as_str().unwrap_or("命令执行失败"),
                        );
                        failure.key = format!("{}:failure", entry.key);
                        entries.push(entry);
                        entries.push(failure);
                    } else {
                        entries.push(entry);
                    }
                }
            }
            if !turn["error"].is_null()
                || matches!(turn["status"].as_str(), Some("failed" | "interrupted"))
            {
                let mut entry = Entry::new(
                    Kind::Error,
                    format!("轮次 {}\n{}", turn["status"], turn["error"]),
                );
                entry.key = serde_json::json!([turn_key, "error"]).to_string();
                entries.push(entry);
            }
        }
    }
    entries
}

impl Entry {
    pub fn label(&self) -> &str {
        self.source_label.as_deref().unwrap_or(match self.kind {
            Kind::User => "你",
            Kind::Assistant => "主控",
            Kind::Task => "子任务",
            Kind::Completion => "轮次结束",
            Kind::Unknown => "来源未知",
            Kind::Tool => "工具",
            Kind::Error => "需要处理",
        })
    }
}

/// 合并尚未进入 Codex 历史的本地投递记录；各前端包括排队、失败、取消的用户输入。
pub(crate) fn conversation(value: &Value, messages: &[Delivery], thread: &str) -> Vec<Entry> {
    let mut entries = transcript(value);
    apply_sources(&mut entries, messages, thread);
    for message in messages.iter().filter(|m| m.thread == thread) {
        let index = entries
            .iter()
            .position(|e| e.id.as_deref() == Some(&message.id));
        let index = index.unwrap_or_else(|| {
            let mut entry = Entry::new(Kind::Unknown, &message.text);
            entry.id = Some(message.id.clone());
            entry.apply_source(message);
            entries.push(entry);
            entries.len() - 1
        });
        let entry = &mut entries[index];
        entry.key = serde_json::json!(["message", message.id]).to_string();
        entry.detail = format!("消息 ID：{}\n投递状态：{}", message.id, message.status);
        if let Some(error) = &message.error {
            let mut entry = Entry::new(Kind::Error, error);
            entry.key = serde_json::json!(["message-error", message.id]).to_string();
            entries.push(entry);
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn queued_sent_and_cancelled_messages_share_identity_and_exclude_other_threads() {
        let mut messages = vec![Delivery {
            id: "msg".into(),
            thread: "main".into(),
            text: "输入".into(),
            source: Some(MessageSource::User),
            status: "queued".into(),
            turn: None,
            error: None,
        }];
        let mut other = messages[0].clone();
        other.id = "child".into();
        other.thread = "task-thread".into();
        other.text = "不能同步的子任务内部消息".into();
        messages.push(other);
        let queued = conversation(&json!({}), &messages, "main");
        assert_eq!(queued.len(), 1);
        assert!(queued[0].detail.contains("queued"));
        messages[0].status = "sent".into();
        let sent = conversation(
            &json!({"thread":{"turns":[{"id":"turn", "items":[{"id":"user", "type":"userMessage","clientId":"msg","content":[{"text":"输入"}]}]}]}}),
            &messages,
            "main",
        );
        assert_eq!(sent.len(), 1);
        assert_eq!(queued[0].key, sent[0].key);
        assert!(sent[0].detail.contains("sent"));
        messages[0].status = "cancelled".into();
        let cancelled = conversation(&json!({}), &messages, "main");
        assert_eq!(cancelled.len(), 1);
        assert!(cancelled[0].detail.contains("cancelled"));
    }
}
