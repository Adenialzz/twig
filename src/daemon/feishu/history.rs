//! 每个已接入单聊独立持久化主控历史同步进度，不读取任何子任务绑定。
use super::*;
use crate::agent::history::{Entry, conversation};
#[cfg(test)]
use crate::agent::history::{apply_sources, transcript};

#[derive(Default, Serialize, Deserialize)]
struct Destination {
    app: String,
    user: String,
    chat: String,
    thread: String,
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
    ) -> Result<()> {
        let texts: Vec<_> = entries
            .iter()
            .map(|entry| (&entry.key, entry.full_text()))
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
            for (entry_key, text) in &texts {
                if destination.seen.get(*entry_key) == Some(text) {
                    continue;
                }
                let update = destination.seen.contains_key(*entry_key);
                let heading = format!(
                    "主控会话 {thread}\n条目 {}{}\n",
                    entry_key,
                    if update { " · 更新" } else { "" }
                );
                destination
                    .outbox
                    .extend(
                        chunks(text)
                            .into_iter()
                            .enumerate()
                            .map(|(part, text)| Reply {
                                text: format!("{heading}分段 {}\n{text}", part + 1),
                                status: "queued".into(),
                            }),
                    );
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
        let entries = conversation(&value, &state.messages, &binding.thread);
        self.observe(config, &inbox.lock().unwrap(), &binding.thread, &entries)?;
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
    fn backfill_updates_and_restart_preserve_each_destination_and_complete_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut inbox = Inbox::open(dir.path().join("inbox.json")).unwrap();
        let config = config();
        inbox.receive(&config, &event("ou_one", "oc_one")).unwrap();
        let file = dir.path().join("history.json");
        let mut history = History::open(file.clone()).unwrap();
        let value = snapshot("一".repeat(5000).as_str());
        let mut entries = transcript(&value);
        apply_sources(
            &mut entries,
            &[Delivery {
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
            }],
            "controller",
        );
        history
            .observe(&config, &inbox, "controller", &entries)
            .unwrap();
        let key = history.destinations.keys().next().unwrap().clone();
        let destination = &history.destinations[&key];
        let texts = destination
            .outbox
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Console 中的旧消息",
            "之前的主控回复",
            "完整补丁",
            "完整 MCP 结果",
            "子任务 · p/t",
            "子任务回报",
        ] {
            assert!(texts.contains(expected), "{expected}");
        }
        assert_eq!(texts.matches('一').count(), 5000);
        let count = destination.outbox.len();
        // 模拟已发送前两段、第三段结果未知，重启不能回退游标。
        history.destinations.get_mut(&key).unwrap().outbox[0].status = "sent".into();
        history.destinations.get_mut(&key).unwrap().outbox[1].status = "sent".into();
        history.destinations.get_mut(&key).unwrap().outbox[2].status = "sending".into();
        history.save().unwrap();
        let mut history = History::open(file).unwrap();
        history
            .observe(&config, &inbox, "controller", &entries)
            .unwrap();
        assert_eq!(history.destinations[&key].outbox.len(), count);
        history
            .send(&config, &key, &AtomicBool::new(false))
            .unwrap(); // 不访问不可达 API。
        assert_eq!(history.destinations[&key].outbox[2].status, "sending");
        let mut updated = entries;
        updated[2].detail.push_str("\n断线后补齐的新输出");
        history
            .observe(&config, &inbox, "controller", &updated)
            .unwrap();
        assert!(
            history.destinations[&key]
                .outbox
                .last()
                .unwrap()
                .text
                .contains("断线后补齐的新输出")
        );
        assert!(
            history.destinations[&key].outbox[count]
                .text
                .contains("更新")
        );
        // 第二个用户独立从完整历史开始，不能借用第一个用户的已发水位。
        inbox.receive(&config, &event("ou_two", "oc_two")).unwrap();
        history
            .observe(&config, &inbox, "controller", &updated)
            .unwrap();
        let second = history
            .destinations
            .values()
            .find(|d| d.user == "ou_two")
            .unwrap();
        assert!(second.outbox.iter().all(|r| r.status == "queued"));
        assert!(second.outbox[0].text.contains("Console 中的旧消息"));
        // 移除允许用户后不为其收集新增内容。
        let config = Config {
            users: HashSet::from(["ou_two".into()]),
            ..config
        };
        let count = history.destinations[&key].outbox.len();
        updated[0].text.push_str("新增");
        history
            .observe(&config, &inbox, "controller", &updated)
            .unwrap();
        assert_eq!(history.destinations[&key].outbox.len(), count);
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
            .observe(&config, &inbox, "controller", &[entry])
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
