//! 飞书新消息出站队列：订阅持久化事件，不读取主控历史。
use super::*;
use crate::agent::{
    Store,
    stream::{Event, Role},
};

#[derive(Serialize, Deserialize)]
struct Destination {
    app: String,
    user: String,
    chat: String,
    cursor: u64,
    joined: u64,
    outbox: Vec<Reply>,
    error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Outbox {
    destinations: BTreeMap<String, Destination>,
    #[serde(skip)]
    file: PathBuf,
    #[serde(skip)]
    dirty: bool,
}
impl Outbox {
    pub fn open(file: PathBuf) -> Result<Self> {
        let mut outbox = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).context("飞书出站记录损坏")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error.into()),
        };
        outbox.file = file;
        Ok(outbox)
    }
    fn save(&self) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(self.file.parent().unwrap())?;
        serde_json::to_writer(&mut file, self)?;
        file.as_file().sync_all()?;
        file.persist(&self.file)?;
        Ok(())
    }
    pub fn subscribe(&mut self, registry: &Path, config: &Config, inbox: &Inbox) -> Result<()> {
        for incoming in inbox
            .messages
            .values()
            .filter(|m| config.users.contains(&m.user))
        {
            let key = json!([config.app, incoming.user, incoming.chat]).to_string();
            if let std::collections::btree_map::Entry::Vacant(entry) = self.destinations.entry(key)
            {
                // 与主控消息入队有序；只读取事件位置，不枚举历史消息。
                let cursor = Store::new(registry)?.subscribe_messages()?;
                entry.insert(Destination {
                    app: config.app.clone(),
                    user: incoming.user.clone(),
                    chat: incoming.chat.clone(),
                    cursor,
                    joined: cursor,
                    outbox: vec![],
                    error: None,
                });
                self.dirty = true;
            }
        }
        self.persist()
    }
    fn persist(&mut self) -> Result<()> {
        if self.dirty {
            self.save()?;
            self.dirty = false;
        }
        Ok(())
    }
    fn append(&mut self, key: &str, events: &[Event]) {
        let destination = self.destinations.get_mut(key).unwrap();
        for event in events {
            destination.cursor = event.sequence;
            if event.origin <= destination.joined {
                continue;
            }
            let text = format!(
                "【{}】\n{}",
                match event.role {
                    Role::User => "User",
                    Role::Codex => "Codex",
                },
                event.text
            );
            let parts = chunks(&text);
            let total = parts.len();
            destination
                .outbox
                .extend(parts.into_iter().enumerate().map(|(part, text)| Reply {
                    text: if total > 1 {
                        format!("{text}\n（{}/{total}）", part + 1)
                    } else {
                        text
                    },
                    status: "queued".into(),
                }));
        }
        self.dirty |= !events.is_empty();
    }
    pub fn sync(&mut self, registry: &Path, config: &Config, stop: &AtomicBool) -> Result<()> {
        let keys: Vec<_> = self
            .destinations
            .iter()
            .filter(|(_, d)| d.app == config.app && config.users.contains(&d.user))
            .map(|(key, _)| key.clone())
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        let store = Store::new(registry)?;
        // 单个新轮次暂时不可读不能阻断已经持久化的事件投递。
        let collected = store.collect_messages();
        let state = store.load()?;
        let stream = state
            .stream
            .as_ref()
            .context("飞书订阅对应的新消息日志丢失")?;
        for key in &keys {
            self.append(key, stream.after(self.destinations[key].cursor)?);
        }
        self.persist()?;
        for key in keys {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Err(error) = self.send(config, &key, stop) {
                self.destinations.get_mut(&key).unwrap().error = Some(format!("{error:#}"));
                self.save()?;
            }
        }
        collected
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
        health.outbox_pending = self
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
        health.outbox_errors = self
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
    #[test]
    fn subscription_starts_at_tail_and_restart_preserves_pending_events() {
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join("config.toml");
        let store = Store::new(&registry).unwrap();
        let config = config();
        let mut inbox = Inbox::open(root.path().join("inbox.json")).unwrap();
        inbox.receive(&config, &event("ou_one", "oc_one")).unwrap();
        let file = root.path().join("outbox.json");
        let mut outbox = Outbox::open(file.clone()).unwrap();
        outbox.subscribe(&registry, &config, &inbox).unwrap();
        let one = outbox.destinations.keys().next().unwrap().clone();
        assert_eq!(outbox.destinations[&one].cursor, 0);
        assert!(outbox.destinations[&one].outbox.is_empty());
        let binding = crate::agent::Binding {
            thread: "controller".into(),
            cwd: root.path().into(),
            project: None,
            task: None,
            stopped: false,
        };
        let mut state = store.load().unwrap();
        state
            .stream
            .as_mut()
            .unwrap()
            .accepted(&binding, "first", "first new input", None);
        fs::write(
            store.dir.join("runtime.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        inbox.receive(&config, &event("ou_two", "oc_two")).unwrap();
        outbox.subscribe(&registry, &config, &inbox).unwrap();
        let two = outbox
            .destinations
            .iter()
            .find(|(_, d)| d.user == "ou_two")
            .unwrap()
            .0
            .clone();
        assert_eq!(outbox.destinations[&two].cursor, 1);
        for key in [&one, &two] {
            outbox.append(
                key,
                state
                    .stream
                    .as_ref()
                    .unwrap()
                    .after(outbox.destinations[key].cursor)
                    .unwrap(),
            );
        }
        outbox.persist().unwrap();
        assert_eq!(outbox.destinations[&one].outbox.len(), 1);
        assert!(outbox.destinations[&two].outbox.is_empty());
        let mut outbox = Outbox::open(file).unwrap();
        outbox.subscribe(&registry, &config, &inbox).unwrap();
        assert_eq!(outbox.destinations[&one].cursor, 1);
        assert_eq!(outbox.destinations[&one].outbox[0].status, "queued");
        assert!(state.stream.as_ref().unwrap().after(2).is_err());
        // 后接入者也不接收旧输入的迟到回复；新输入及回复仍正常消费。
        for key in [&one, &two] {
            outbox.append(
                key,
                &[
                    Event {
                        sequence: 2,
                        origin: 1,
                        role: Role::Codex,
                        text: "reply to first".into(),
                    },
                    Event {
                        sequence: 3,
                        origin: 3,
                        role: Role::User,
                        text: "new input after both joined".into(),
                    },
                ],
            );
        }
        assert_eq!(outbox.destinations[&one].outbox.len(), 3);
        assert_eq!(outbox.destinations[&two].outbox.len(), 1);
        assert_eq!(outbox.destinations[&two].cursor, 3);
        assert!(
            outbox.destinations[&two].outbox[0]
                .text
                .contains("after both joined")
        );
    }
    #[test]
    fn failed_subscription_save_must_retry_before_accepting_input() {
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join("config.toml");
        let config = config();
        let mut inbox = Inbox::open(root.path().join("inbox.json")).unwrap();
        inbox.receive(&config, &event("ou_one", "oc_one")).unwrap();
        let file = root.path().join("outbox.json");
        let mut outbox = Outbox::open(file.clone()).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(outbox.subscribe(&registry, &config, &inbox).is_err());
        assert!(outbox.subscribe(&registry, &config, &inbox).is_err());
        fs::remove_dir(&file).unwrap();
        outbox.subscribe(&registry, &config, &inbox).unwrap();
        assert_eq!(Outbox::open(file).unwrap().destinations.len(), 1);
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
        let file = dir.path().join("outbox.json");
        let registry = dir.path().join("config.toml");
        let mut outbox = Outbox::open(file.clone()).unwrap();
        outbox.subscribe(&registry, &config, &inbox).unwrap();
        for key in outbox.destinations.keys().cloned().collect::<Vec<_>>() {
            outbox.append(
                &key,
                &[Event {
                    sequence: 1,
                    origin: 1,
                    role: Role::Codex,
                    text: "完整回复".into(),
                }],
            );
        }
        outbox.persist().unwrap();
        let keys: Vec<_> = outbox.destinations.keys().cloned().collect();
        assert!(
            outbox
                .send(&config, &keys[0], &AtomicBool::new(false))
                .is_err()
        );
        let mut outbox = Outbox::open(file.clone()).unwrap();
        // 不发送未知请求；继续另一个目的地。
        outbox
            .send(&config, &keys[0], &AtomicBool::new(false))
            .unwrap();
        outbox
            .send(&config, &keys[1], &AtomicBool::new(false))
            .unwrap();
        server.join().unwrap();
        let mut health = Health::default();
        Outbox::open(file).unwrap().health(&mut health);
        assert_eq!(health.unknown_replies, 1);
        assert_eq!(health.outbox_pending, 0);
    }

    #[test]
    fn chunks_round_trip_multibyte_text_and_control_characters_without_truncation() {
        let text = "中😀a\n\t\"".repeat(4000);
        let parts = chunks(&text);
        assert!(parts.iter().all(|s| s.len() <= 3000));
        assert_eq!(parts.concat(), text);
    }
}
