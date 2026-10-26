//! 每个已接入单聊独立持久化主控历史同步进度，不读取任何子任务绑定。
use super::*;
use crate::agent::Delivery;
use crate::agent::history::{Entry, Kind, apply_sources, transcript};

#[derive(Default, Serialize, Deserialize)]
struct Destination {
    app: String,
    user: String,
    chat: String,
    thread: String,
    #[serde(default)]
    compact: bool,
    seen: BTreeMap<String, String>,
    outbox: Vec<Reply>,
    error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub(super) struct History {
    destinations: BTreeMap<String, Destination>,
    #[serde(skip)]
    file: PathBuf,
}
impl History {
    pub fn open(file: PathBuf) -> Result<Self> {
        let mut history = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).context("飞书历史同步记录损坏")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.into()),
        };
        history.file = file;
        Ok(history)
    }
    fn save(&self) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(self.file.parent().unwrap())?;
        serde_json::to_writer(&mut file, self)?;
        file.as_file().sync_all()?;
        file.persist(&self.file)?;
        Ok(())
    }
    fn observe(
        &mut self,
        config: &Config,
        inbox: &Inbox,
        thread: &str,
        entries: &[Entry],
        latest: &HashSet<String>,
    ) -> Result<()> {
        let texts: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    &entry.key,
                    format!(
                        "【{}】\n{}",
                        if entry.kind == Kind::Assistant {
                            "Codex"
                        } else {
                            "User"
                        },
                        entry.text
                    ),
                )
            })
            .collect();
        let mut visited = HashSet::new();
        let mut changed = false;
        for incoming in inbox.messages.values() {
            if !config.users.contains(&incoming.user) {
                continue;
            }
            let key = json!([config.app, incoming.user, incoming.chat, thread]).to_string();
            if !visited.insert(key.clone()) {
                continue;
            }
            let destination = self.destinations.entry(key).or_insert_with(|| Destination {
                app: config.app.clone(),
                user: incoming.user.clone(),
                chat: incoming.chat.clone(),
                thread: thread.into(),
                ..Default::default()
            });
            if !destination.compact {
                // 升级只丢弃尚未发送的旧展示队列；未知发送必须保留并阻塞，不能重试。
                destination.outbox.retain(|reply| reply.status != "queued");
                destination.seen = texts
                    .iter()
                    .filter(|(key, _)| !latest.contains(*key))
                    .map(|(key, text)| ((*key).clone(), text.clone()))
                    .collect();
                destination.compact = true;
                changed = true;
            }
            for (entry_key, text) in &texts {
                if destination.seen.get(*entry_key) == Some(text) {
                    continue;
                }
                let update = destination.seen.contains_key(*entry_key);
                let parts = chunks(text);
                let total = parts.len();
                destination
                    .outbox
                    .extend(parts.into_iter().enumerate().map(|(part, text)| Reply {
                        text: format!(
                            "{}{}{}",
                            if update { "【更新】\n" } else { "" },
                            text,
                            if total > 1 {
                                format!("\n（{}/{total}）", part + 1)
                            } else {
                                String::new()
                            }
                        ),
                        status: "queued".into(),
                    }));
                destination.seen.insert((*entry_key).clone(), text.clone());
                changed = true;
            }
        }
        // 观察水位与全部待发分段在同一次原子替换中提交，然后才允许发送。
        if changed {
            self.save()?;
        }
        Ok(())
    }
    pub fn sync(
        &mut self,
        registry: &Path,
        config: &Config,
        inbox: &Mutex<Inbox>,
        stop: &AtomicBool,
    ) -> Result<()> {
        if !inbox
            .lock()
            .unwrap()
            .messages
            .values()
            .any(|m| config.users.contains(&m.user))
        {
            return Ok(());
        }
        let store = crate::agent::Store::new(registry)?;
        let state = store.load()?;
        let Some(binding) = state.bindings.get("controller") else {
            return Ok(());
        };
        ensure!(
            binding.project.is_none() && binding.task.is_none(),
            "飞书历史只能同步主控绑定"
        );
        let value = store.read(binding)?;
        let (entries, latest) = compact_entries(&value, &state.messages, &binding.thread);
        self.observe(
            config,
            &inbox.lock().unwrap(),
            &binding.thread,
            &entries,
            &latest,
        )?;
        let keys: Vec<_> = self
            .destinations
            .iter()
            .filter(|(_, d)| {
                d.app == config.app && config.users.contains(&d.user) && d.thread == binding.thread
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Err(error) = self.send(config, &key, stop) {
                self.destinations.get_mut(&key).unwrap().error = Some(format!("{error:#}"));
                self.save()?;
            }
        }
        Ok(())
    }
    fn send(&mut self, config: &Config, key: &str, stop: &AtomicBool) -> Result<()> {
        // 每轮每个单聊有界发送；结果未知只阻塞这个目的地，不越过缺口。
        for _ in 0..8 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let destination = &self.destinations[key];
            let Some(index) = destination.outbox.iter().position(|r| r.status != "sent") else {
                break;
            };
            if destination.outbox[index].status == "sending" {
                break;
            }
            let token = token(config)?;
            let chat = destination.chat.clone();
            let text = destination.outbox[index].text.clone();
            self.destinations.get_mut(key).unwrap().outbox[index].status = "sending".into();
            self.save()?;
            post(
                config,
                "/open-apis/im/v1/messages?receive_id_type=chat_id",
                Some(&token),
                json!({"receive_id":chat,"msg_type":"text","content":serde_json::to_string(&json!({"text":text}))?}),
            )?;
            let destination = self.destinations.get_mut(key).unwrap();
            destination.outbox[index].status = "sent".into();
            destination.error = None;
            self.save()?;
            thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }
    pub fn health(&self, health: &mut Health) {
        health.history_pending = self
            .destinations
            .values()
            .flat_map(|d| &d.outbox)
            .filter(|r| r.status == "queued")
            .count();
        health.unknown_replies += self
            .destinations
            .values()
            .flat_map(|d| &d.outbox)
            .filter(|r| r.status == "sending")
            .count();
        health.history_errors = self
            .destinations
            .iter()
            .filter_map(|(key, d)| d.error.as_ref().map(|e| (key.clone(), e.clone())))
            .collect();
    }
}

/// 只投影用户输入及已完成轮次的最终回答，不改动 Console 的完整历史。
fn compact_entries(
    value: &Value,
    messages: &[Delivery],
    thread: &str,
) -> (Vec<Entry>, HashSet<String>) {
    let mut entries = Vec::new();
    let mut latest = HashSet::new();
    if let Some(turns) = value["thread"]["turns"].as_array() {
        for turn in turns {
            let mut current = transcript(&json!({"thread":{"turns":[turn]}}));
            let final_id = if turn["status"] == "completed" {
                turn["items"]
                    .as_array()
                    .and_then(|items| {
                        items.iter().rev().find(|item| {
                            item["type"] == "agentMessage" && item["phase"] != "commentary"
                        })
                    })
                    .and_then(|item| item["id"].as_str())
            } else {
                None
            };
            let final_key = final_id.map(|id| json!([turn["id"], id]).to_string());
            current.retain(|entry| match entry.kind {
                Kind::User | Kind::Unknown => true,
                Kind::Assistant => final_key.as_ref() == Some(&entry.key),
                _ => false,
            });
            for entry in &mut current {
                if let Some(id) = &entry.id {
                    entry.key = json!(["message", id]).to_string();
                }
            }
            latest = current.iter().map(|entry| entry.key.clone()).collect();
            entries.extend(current);
        }
    }
    apply_sources(&mut entries, messages, thread);
    entries.retain(|entry| {
        let visible = matches!(entry.kind, Kind::User | Kind::Unknown | Kind::Assistant);
        if !visible {
            latest.remove(&entry.key);
        }
        visible
    });
    (entries, latest)
}

fn chunks(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut remaining = text;
    // 按 UTF-8 字节限制，保留全部文本，给 JSON 转义及来源标签留出空间。
    while !remaining.is_empty() {
        let mut end = remaining.len().min(3000);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Delivery, MessageSource};

    fn config() -> Config {
        Config {
            app: "app".into(),
            secret: "secret".into(),
            users: HashSet::from(["ou_one".into(), "ou_two".into()]),
            base: "http://127.0.0.1:1".into(),
        }
    }
    fn event(user: &str, chat: &str) -> Value {
        json!({"header":{"event_type":"im.message.receive_v1"},"event":{"sender":{"sender_type":"user","sender_id":{"open_id":user}},"message":{"message_id":format!("om_{user}"),"chat_id":chat,"chat_type":"p2p","message_type":"text","content":"{\"text\":\"hello\"}"}}})
    }
    fn snapshot(output: &str) -> Value {
        json!({"thread":{"id":"controller","turns":[{"id":"turn-a","status":"completed","items":[
            {"id":"user-a","type":"userMessage","clientId":"cli-a","content":[{"type":"text","text":"Console 中的旧消息"}]},
            {"id":"agent-a","type":"agentMessage","text":"之前的主控回复"},
            {"id":"tool-a","type":"commandExecution","command":"echo test","status":"completed","aggregatedOutput":output},
            {"id":"file-a","type":"fileChange","changes":[{"path":"x","diff":"完整补丁"}]},
            {"id":"mcp-a","type":"mcpToolCall","result":{"content":"完整 MCP 结果"}},
            {"id":"task-a","type":"userMessage","clientId":"task-report","content":[{"text":"子任务回报"}]}
        ]}]}})
    }
    #[test]
    fn compact_projection_waits_for_final_and_hides_internal_entries() {
        let mut value = snapshot("tool output");
        value["thread"]["turns"][0]["items"].as_array_mut().unwrap().extend([
            json!({"id":"progress","type":"agentMessage","phase":"commentary","text":"working"}),
            json!({"id":"final","type":"agentMessage","phase":"final_answer","text":"done"}),
        ]);
        let messages = vec![Delivery {
            id: "task-report".into(),
            thread: "controller".into(),
            text: "子任务回报".into(),
            source: Some(MessageSource::Task {
                project: "p".into(),
                task: "t".into(),
            }),
            status: "sent".into(),
            turn: Some("turn-a".into()),
            error: None,
        }];
        value["thread"]["turns"][0]["status"] = json!("inProgress");
        let (active, _) = compact_entries(&value, &messages, "controller");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].text, "Console 中的旧消息");
        value["thread"]["turns"][0]["status"] = json!("completed");
        let (done, _) = compact_entries(&value, &messages, "controller");
        assert_eq!(done.len(), 2);
        assert_eq!(done[1].text, "done");
        // 旧协议没有 phase 时，仅使用已完成轮次最后一条回答。
        value["thread"]["turns"][0]["items"]
            .as_array_mut()
            .unwrap()
            .pop();
        let (done, _) = compact_entries(&value, &messages, "controller");
        assert_eq!(done[1].text, "之前的主控回复");
    }

    #[test]
    fn latest_backfill_incremental_sync_upgrade_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let config = config();
        let mut inbox = Inbox::open(dir.path().join("inbox.json")).unwrap();
        inbox.receive(&config, &event("ou_one", "oc_one")).unwrap();
        let file = dir.path().join("history.json");
        let mut history = History::open(file.clone()).unwrap();
        let mut value = snapshot("hidden tool");
        value["thread"]["turns"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id":"latest", "status":"inProgress", "items":[
                    {"id":"u","type":"userMessage","content":[{"text":"new input"}]},
                    {"id":"a","type":"agentMessage","text":"partial"}
                ]
            }));
        let (entries, latest) = compact_entries(&value, &[], "controller");
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        let key = history.destinations.keys().next().unwrap().clone();
        assert_eq!(history.destinations[&key].outbox.len(), 1);
        assert_eq!(
            history.destinations[&key].outbox[0].text,
            "【User】\nnew input"
        );
        history.destinations.get_mut(&key).unwrap().outbox[0].status = "sent".into();
        history.save().unwrap();
        let mut history = History::open(file.clone()).unwrap();
        value["thread"]["turns"][1]["status"] = json!("completed");
        value["thread"]["turns"][1]["items"][1]["text"] = json!("final reply");
        let (entries, latest) = compact_entries(&value, &[], "controller");
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        assert_eq!(history.destinations[&key].outbox.len(), 2);
        assert_eq!(
            history.destinations[&key].outbox[1].text,
            "【Codex】\nfinal reply"
        );
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        assert_eq!(history.destinations[&key].outbox.len(), 2);
        // 迁移旧持久化格式：保留成功/未知，清掉旧待发工具，再仅补最近一轮。
        let d = history.destinations.get_mut(&key).unwrap();
        d.outbox[1].status = "sending".into();
        d.outbox.push(Reply {
            text: "old tool".into(),
            status: "queued".into(),
        });
        let mut disk = serde_json::to_value(&history).unwrap();
        disk["destinations"][&key]
            .as_object_mut()
            .unwrap()
            .remove("compact");
        fs::write(&file, serde_json::to_vec(&disk).unwrap()).unwrap();
        let mut history = History::open(file).unwrap();
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        let d = &history.destinations[&key];
        assert_eq!(d.outbox[0].status, "sent");
        assert_eq!(d.outbox[1].status, "sending");
        assert!(!d.outbox.iter().any(|r| r.text == "old tool"));
        history
            .send(&config, &key, &AtomicBool::new(false))
            .unwrap();
        inbox.receive(&config, &event("ou_two", "oc_two")).unwrap();
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        let second = history
            .destinations
            .values()
            .find(|d| d.user == "ou_two")
            .unwrap();
        assert_eq!(second.outbox.len(), 2);
        // 已接入者重启后补齐所有离线新轮次，不再次裁成最近一轮。
        let mut history = History::open(dir.path().join("history.json")).unwrap();
        let config = Config {
            users: HashSet::from(["ou_two".into()]),
            ..config
        };
        let first_count = history.destinations[&key].outbox.len();
        for id in ["offline-one", "offline-two"] {
            value["thread"]["turns"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "id":id, "status":"completed", "items":[
                        {"id":"input","type":"userMessage","content":[{"text":id}]},
                        {"id":"reply","type":"agentMessage","text":format!("reply {id}")}
                    ]
                }));
        }
        let (mut entries, latest) = compact_entries(&value, &[], "controller");
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        assert_eq!(history.destinations[&key].outbox.len(), first_count);
        let second = history
            .destinations
            .values()
            .find(|d| d.user == "ou_two")
            .unwrap();
        assert_eq!(second.outbox.len(), 6);
        entries.last_mut().unwrap().text = "corrected reply".into();
        history
            .observe(&config, &inbox, "controller", &entries, &latest)
            .unwrap();
        let second = history
            .destinations
            .values()
            .find(|d| d.user == "ou_two")
            .unwrap();
        assert_eq!(second.outbox.len(), 7);
        assert!(second.outbox.last().unwrap().text.contains("【更新】"));
    }
    #[test]
    fn lost_http_reply_stays_unknown_after_restart_and_other_destination_continues() {
        use super::super::transport_tests::{read_http, respond};
        use std::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = Config {
            base: format!("http://127.0.0.1:{}", http.local_addr().unwrap().port()),
            ..config()
        };
        let server = thread::spawn(move || {
            for destination in 0..2 {
                let (mut stream, _) = http.accept().unwrap();
                assert!(read_http(&mut stream).contains("tenant_access_token"));
                respond(&mut stream, json!({"code":0,"tenant_access_token":"token"}));
                let (mut stream, _) = http.accept().unwrap();
                let request = read_http(&mut stream);
                if destination == 0 {
                    assert!(request.contains("oc_one"));
                    // 服务已经收到请求，但响应丢失。
                } else {
                    assert!(request.contains("oc_two"));
                    respond(&mut stream, json!({"code":0}));
                }
            }
        });
        let mut inbox = Inbox::open(dir.path().join("inbox.json")).unwrap();
        inbox.receive(&config, &event("ou_one", "oc_one")).unwrap();
        inbox.receive(&config, &event("ou_two", "oc_two")).unwrap();
        let file = dir.path().join("history.json");
        let mut history = History::open(file.clone()).unwrap();
        let mut entry = Entry::new(crate::agent::history::Kind::Assistant, "完整回复");
        entry.key = "item".into();
        history
            .observe(
                &config,
                &inbox,
                "controller",
                &[entry],
                &HashSet::from(["item".into()]),
            )
            .unwrap();
        let keys: Vec<_> = history.destinations.keys().cloned().collect();
        assert!(
            history
                .send(&config, &keys[0], &AtomicBool::new(false))
                .is_err()
        );
        let mut history = History::open(file.clone()).unwrap();
        // 不发送未知请求；继续另一个目的地。
        history
            .send(&config, &keys[0], &AtomicBool::new(false))
            .unwrap();
        history
            .send(&config, &keys[1], &AtomicBool::new(false))
            .unwrap();
        server.join().unwrap();
        let mut health = Health::default();
        History::open(file).unwrap().health(&mut health);
        assert_eq!(health.unknown_replies, 1);
        assert_eq!(health.history_pending, 0);
    }

    #[test]
    fn chunks_round_trip_multibyte_text_and_control_characters_without_truncation() {
        let text = "中😀a\n\t\"".repeat(4000);
        let parts = chunks(&text);
        assert!(parts.iter().all(|s| s.len() <= 3000));
        assert_eq!(parts.concat(), text);
    }
}
