//! 飞书应用机器人：官方长连接帧协议；事件落盘后确认，投递与回复独立推进。
//! 协议来源：https://github.com/larksuite/oapi-sdk-python/tree/v2_main/lark_oapi/ws
mod history;

use anyhow::{Context, Result, ensure};
use prost::Message as ProtoMessage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Connector, Message, stream::MaybeTlsStream};

fn tls_client(roots: rustls::RootCertStore) -> Result<Arc<rustls::ClientConfig>> {
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    ))
}

fn native_tls_client() -> Result<Arc<rustls::ClientConfig>> {
    let certificates = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(certificates.certs);
    ensure!(
        added > 0,
        "未找到可用的系统 TLS 根证书：{:?}",
        certificates.errors
    );
    tls_client(roots).context("初始化飞书 TLS 失败")
}

#[derive(Clone)]
struct Config {
    app: String,
    secret: String,
    users: HashSet<String>,
    base: String,
}
impl Config {
    fn from_env() -> Result<Option<Self>> {
        let app = std::env::var("TWIG_FEISHU_APP_ID").ok();
        let secret = std::env::var("TWIG_FEISHU_APP_SECRET").ok();
        let users = std::env::var("TWIG_FEISHU_ALLOWED_USERS").ok();
        if app.is_none() && secret.is_none() && users.is_none() {
            return Ok(None);
        }
        let app = app.context("请配置 TWIG_FEISHU_APP_ID")?;
        let secret = secret.context("请配置 TWIG_FEISHU_APP_SECRET")?;
        let users: HashSet<_> = users
            .context("请配置 TWIG_FEISHU_ALLOWED_USERS（open_id，逗号分隔）")?
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        ensure!(
            !app.trim().is_empty() && !secret.trim().is_empty() && !users.is_empty(),
            "飞书应用凭证与允许用户列表不能为空"
        );
        let base = std::env::var("TWIG_FEISHU_API_BASE")
            .unwrap_or_else(|_| "https://open.feishu.cn".into());
        ensure!(
            base == "https://open.feishu.cn" || base.starts_with("http://127.0.0.1:"),
            "飞书 API 只允许官方地址或本地测试服务"
        );
        Ok(Some(Self {
            app,
            secret,
            users,
            base,
        }))
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Reply {
    text: String,
    status: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Incoming {
    message: String,
    user: String,
    chat: String,
    text: String,
    delivery: String,
    thread: Option<String>,
    replies: Vec<Reply>,
    finished: bool,
    error: Option<String>,
}
struct Inbox {
    file: PathBuf,
    messages: BTreeMap<String, Incoming>,
}
impl Inbox {
    fn open(file: PathBuf) -> Result<Self> {
        let messages = match fs::read(&file) {
            Ok(data) => serde_json::from_slice(&data).context("飞书接收记录损坏")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { file, messages })
    }
    fn save(&self) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(self.file.parent().unwrap())?;
        serde_json::to_writer(&mut file, &self.messages)?;
        file.as_file().sync_all()?;
        file.persist(&self.file)?;
        Ok(())
    }
    fn receive(&mut self, config: &Config, event: &Value) -> Result<()> {
        if event["header"]["event_type"] != "im.message.receive_v1" {
            return Ok(());
        }
        let event = &event["event"];
        let sender = &event["sender"];
        let message = &event["message"];
        if sender["sender_type"] != "user" || message["chat_type"] != "p2p" {
            return Ok(());
        }
        let user = sender["sender_id"]["open_id"]
            .as_str()
            .context("飞书用户缺少 open_id")?;
        if !config.users.contains(user) {
            return Ok(());
        }
        if message["message_type"] != "text" {
            return Ok(());
        }
        let id = message["message_id"]
            .as_str()
            .context("飞书事件缺少消息 ID")?;
        if self.messages.contains_key(id) {
            return Ok(());
        }
        let content: Value =
            serde_json::from_str(message["content"].as_str().context("飞书消息缺少内容")?)?;
        let text = content["text"].as_str().context("飞书消息缺少文本")?;
        ensure!(!text.trim().is_empty(), "飞书消息不能为空");
        self.messages.insert(
            id.into(),
            Incoming {
                message: id.into(),
                user: user.into(),
                chat: message["chat_id"]
                    .as_str()
                    .context("飞书消息缺少会话")?
                    .into(),
                text: text.into(),
                delivery: crate::agent::message_id(),
                thread: None,
                replies: vec![Reply {
                    text: "已收到，正在交给主控；主控忙时会排队。".into(),
                    status: "queued".into(),
                }],
                finished: false,
                error: None,
            },
        );
        self.save()
    }
}

#[derive(Clone, Default, Serialize)]
pub struct Health {
    pub enabled: bool,
    pub connected: bool,
    pub error: Option<String>,
    pub pending: usize,
    pub unknown_replies: usize,
    pub message_errors: BTreeMap<String, String>,
    pub history_pending: usize,
    pub history_errors: BTreeMap<String, String>,
    pub history_error: Option<String>,
    pub worker_errors: BTreeMap<String, String>,
}
pub struct Runtime {
    stop: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
    pub health: Arc<Mutex<Health>>,
}
impl Runtime {
    pub fn start(registry: &Path, dir: &Path) -> Result<Self> {
        let config = Config::from_env()?;
        let health = Arc::new(Mutex::new(Health {
            enabled: config.is_some(),
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers = vec![];
        if let Some(config) = config {
            // TLS 初始化在启动线程前完成，配置错误直接报告，不能留下假在线的后台。
            let tls = native_tls_client()?;
            let inbox = Arc::new(Mutex::new(Inbox::open(dir.join("feishu.json"))?));
            let mut history = history::History::open(dir.join("feishu-history.json"))?;
            let config = Arc::new(config);
            let rx_config = config.clone();
            let rx_inbox = inbox.clone();
            let rx_stop = stop.clone();
            let rx_health = health.clone();
            workers.push(spawn_worker(
                "接收",
                rx_stop.clone(),
                rx_health.clone(),
                move || {
                    let mut delay = 2;
                    while !rx_stop.load(Ordering::Relaxed) {
                        let result =
                            receive_loop(&rx_config, &rx_inbox, &rx_stop, &rx_health, &tls);
                        let mut health = rx_health.lock().unwrap();
                        health.connected = false;
                        health.error = result.err().map(|e| format!("{e:#}"));
                        drop(health);
                        sleep(&rx_stop, delay);
                        delay = (delay * 2).min(60);
                    }
                },
            ));
            let tx_stop = stop.clone();
            let tx_health = health.clone();
            let registry = registry.to_owned();
            workers.push(spawn_worker(
                "发送",
                tx_stop.clone(),
                tx_health.clone(),
                move || {
                    while !tx_stop.load(Ordering::Relaxed) {
                        let ids: Vec<_> = inbox
                            .lock()
                            .unwrap()
                            .messages
                            .iter()
                            .filter(|(_, m)| config.users.contains(&m.user))
                            .map(|(id, _)| id.clone())
                            .collect();
                        for id in ids {
                            if tx_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            if let Err(error) = dispatch(&registry, &config, &inbox, &id) {
                                let mut inbox = inbox.lock().unwrap();
                                inbox.messages.get_mut(&id).unwrap().error =
                                    Some(format!("{error:#}"));
                                if let Err(error) = inbox.save() {
                                    tx_health.lock().unwrap().error =
                                        Some(format!("飞书状态保存失败：{error:#}"));
                                }
                            }
                        }
                        let history_error = history
                            .sync(&registry, &config, &inbox, &tx_stop)
                            .err()
                            .map(|error| format!("{error:#}"));
                        let inbox = inbox.lock().unwrap();
                        let mut health = tx_health.lock().unwrap();
                        health.history_error = history_error;
                        health.message_errors = inbox
                            .messages
                            .iter()
                            .filter_map(|(id, m)| m.error.as_ref().map(|e| (id.clone(), e.clone())))
                            .collect();
                        health.pending = inbox
                            .messages
                            .values()
                            .filter(|m| {
                                !m.finished || m.replies.iter().any(|r| r.status == "queued")
                            })
                            .count();
                        health.unknown_replies = inbox
                            .messages
                            .values()
                            .flat_map(|m| &m.replies)
                            .filter(|r| r.status == "sending")
                            .count();
                        history.health(&mut health);
                        drop(health);
                        drop(inbox);
                        sleep(&tx_stop, 2);
                    }
                },
            ));
        }
        Ok(Self {
            stop,
            workers,
            health,
        })
    }
}

// 不捕获并重试 panic：保留原始崩溃日志，同时让健康接口反映线程已经退出。
struct WorkerExit {
    name: &'static str,
    stop: Arc<AtomicBool>,
    health: Arc<Mutex<Health>>,
}
impl Drop for WorkerExit {
    fn drop(&mut self) {
        if thread::panicking() || !self.stop.load(Ordering::Relaxed) {
            let error = format!(
                "飞书{}线程异常退出，请查看 daemon.log 或服务日志并重启 Twig 后台",
                self.name
            );
            // panic 时业务代码可能正持有此锁；仅恢复锁以写入失败诊断，不恢复业务。
            let mut health = self
                .health
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self.name == "接收" {
                health.connected = false;
                health.error = Some(error.clone());
            }
            health.worker_errors.insert(self.name.into(), error.clone());
            self.health.clear_poison();
            super::log::error(&error);
        }
    }
}
fn spawn_worker(
    name: &'static str,
    stop: Arc<AtomicBool>,
    health: Arc<Mutex<Health>>,
    work: impl FnOnce() + Send + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _exit = WorkerExit { name, stop, health };
        work();
    })
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
fn sleep(stop: &AtomicBool, seconds: u64) {
    for _ in 0..seconds * 10 {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn post(config: &Config, path: &str, token: Option<&str>, body: Value) -> Result<Value> {
    let mut file = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(&mut file, &body)?;
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--max-time",
            "15",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
        ])
        .arg(format!("@{}", file.path().display()))
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("飞书 HTTP 需要 curl")?;
    let mut input = child.stdin.take().unwrap();
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    ensure!(
        !config.base.contains(['\r', '\n']) && !token.is_some_and(|t| t.contains(['\r', '\n'])),
        "飞书请求配置含无效换行"
    );
    writeln!(
        input,
        "url = \"{}\"",
        quote(&format!("{}{path}", config.base))
    )?;
    if let Some(token) = token {
        writeln!(input, "header = \"Authorization: Bearer {}\"", quote(token))?;
    }
    drop(input);
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "飞书 HTTP 请求失败或超时；发送请求可能已送达，需核对"
    );
    let value: Value = serde_json::from_slice(&output.stdout).context("飞书响应不是 JSON")?;
    ensure!(
        value["code"] == 0,
        "飞书请求被拒绝，错误码 {}",
        value["code"]
    );
    Ok(value)
}
fn token(config: &Config) -> Result<String> {
    let result = post(
        config,
        "/open-apis/auth/v3/tenant_access_token/internal",
        None,
        json!({"app_id":config.app,"app_secret":config.secret}),
    )?;
    Ok(result["tenant_access_token"]
        .as_str()
        .context("飞书未返回访问令牌")?
        .into())
}
fn dispatch(registry: &Path, config: &Config, inbox: &Mutex<Inbox>, id: &str) -> Result<()> {
    let mut incoming = inbox.lock().unwrap().messages[id].clone();
    if incoming.finished
        && !incoming
            .replies
            .iter()
            .any(|reply| reply.status == "queued")
    {
        return Ok(());
    }
    let store = crate::agent::Store::new(registry)?;
    if incoming.thread.is_none() {
        let binding = store.initialize()?;
        // 绑定先落盘；崩溃后仍使用同一 thread 与消息 ID，不能误发给新主控。
        incoming.thread = Some(binding.thread.clone());
        let mut inbox = inbox.lock().unwrap();
        inbox.messages.insert(id.into(), incoming.clone());
        inbox.save()?;
    }
    if !incoming.finished {
        let state = store.load()?;
        let binding = state.bindings.get("controller").context("主控绑定不存在")?;
        ensure!(
            Some(&binding.thread) == incoming.thread.as_ref(),
            "主控绑定已变化，飞书请求需人工核对"
        );
        store.enqueue_with_id(
            binding,
            incoming.text.clone(),
            incoming.delivery.clone(),
            Some(crate::agent::MessageSource::Feishu {
                user: incoming.user.clone(),
            }),
        )?;
        let state = store.load()?;
        let delivery = state
            .messages
            .iter()
            .find(|m| m.id == incoming.delivery)
            .context("飞书投递记录不存在")?;
        let mut result = None;
        if matches!(delivery.status.as_str(), "failed" | "cancelled") {
            result = Some(format!(
                "请求{}：{}",
                delivery.status,
                delivery.error.as_deref().unwrap_or("请检查 Twig 状态")
            ));
        }
        if let Some(turn_id) = &delivery.turn {
            let history = store.read(binding)?;
            if let Some(turn) = history["thread"]["turns"]
                .as_array()
                .and_then(|turns| turns.iter().find(|turn| turn["id"] == *turn_id))
            {
                match turn["status"].as_str() {
                    Some("completed") => {
                        // 文本及工具输出由完整历史同步发送，不再另外发送最终回复。
                        incoming.finished = true;
                    }
                    Some("failed" | "interrupted") => {
                        result = Some("本轮失败或被中断，请检查 Twig Agent。".into())
                    }
                    _ => {}
                }
            }
        }
        if let Some(text) = result {
            incoming.finished = true;
            for chunk in text.chars().collect::<Vec<_>>().chunks(4000) {
                incoming.replies.push(Reply {
                    text: chunk.iter().collect(),
                    status: "queued".into(),
                });
            }
            let mut inbox = inbox.lock().unwrap();
            inbox.messages.insert(id.into(), incoming.clone());
            inbox.save()?;
        }
    }
    {
        let mut inbox = inbox.lock().unwrap();
        inbox.messages.insert(id.into(), incoming.clone());
        inbox.save()?;
    }
    for index in 0..incoming.replies.len() {
        if incoming.replies[index].status == "sending" {
            break;
        }
        if incoming.replies[index].status != "queued" {
            continue;
        }
        let token = token(config)?;
        incoming.replies[index].status = "sending".into();
        {
            let mut inbox = inbox.lock().unwrap();
            inbox.messages.insert(id.into(), incoming.clone());
            inbox.save()?;
        }
        // 不自动重试结果未知的写请求；sending 跨重启保留供诊断。
        let path = format!("/open-apis/im/v1/messages/{}/reply", incoming.message);
        post(
            config,
            &path,
            Some(&token),
            json!({"msg_type":"text","content":serde_json::to_string(&json!({"text":incoming.replies[index].text}))?}),
        )?;
        incoming.replies[index].status = "sent".into();
        incoming.error = None;
        let mut inbox = inbox.lock().unwrap();
        inbox.messages.insert(id.into(), incoming.clone());
        inbox.save()?;
    }
    Ok(())
}

#[derive(Clone, PartialEq, prost::Message)]
struct Header {
    #[prost(string, required, tag = "1")]
    key: String,
    #[prost(string, required, tag = "2")]
    value: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct Frame {
    #[prost(uint64, required, tag = "1")]
    seq: u64,
    #[prost(uint64, required, tag = "2")]
    log: u64,
    #[prost(int32, required, tag = "3")]
    service: i32,
    #[prost(int32, required, tag = "4")]
    method: i32,
    #[prost(message, repeated, tag = "5")]
    headers: Vec<Header>,
    #[prost(string, optional, tag = "6")]
    encoding: Option<String>,
    #[prost(string, optional, tag = "7")]
    payload_type: Option<String>,
    #[prost(bytes = "vec", optional, tag = "8")]
    payload: Option<Vec<u8>>,
    #[prost(string, optional, tag = "9")]
    log_new: Option<String>,
}
impl Frame {
    fn header(&self, key: &str) -> Result<&str> {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .context("飞书帧缺少头字段")
    }
}
struct Parts {
    at: Instant,
    parts: Vec<Option<Vec<u8>>>,
}
fn receive_loop(
    config: &Config,
    inbox: &Mutex<Inbox>,
    stop: &AtomicBool,
    health: &Mutex<Health>,
    tls: &Arc<rustls::ClientConfig>,
) -> Result<()> {
    let endpoint = post(
        config,
        "/callback/ws/endpoint",
        None,
        json!({"AppID":config.app,"AppSecret":config.secret}),
    )?;
    let url = endpoint["data"]["URL"]
        .as_str()
        .context("飞书未返回长连接地址")?;
    let uri: tungstenite::http::Uri = url.parse().context("飞书长连接地址格式错误")?;
    ensure!(
        uri.scheme_str() == Some("wss")
            || (config.base.starts_with("http://127.0.0.1:")
                && uri.scheme_str() == Some("ws")
                && uri.host() == Some("127.0.0.1")),
        "飞书长连接必须使用 TLS"
    );
    let host = uri.host().context("长连接缺少主机")?;
    let port = uri.port_u16().unwrap_or(443);
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .context("无法解析飞书主机")?;
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .context("飞书长连接 TCP 连接失败")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (mut ws, _) = tungstenite::client_tls_with_config(
        url,
        stream,
        None,
        Some(Connector::Rustls(tls.clone())),
    )
    .map_err(|_| anyhow::anyhow!("飞书长连接握手失败，请核对应用凭证与网络"))?;
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(Duration::from_millis(500)))?,
        MaybeTlsStream::Rustls(s) => s.sock.set_read_timeout(Some(Duration::from_millis(500)))?,
        _ => anyhow::bail!("不支持的飞书 TLS 连接类型"),
    }
    let service: i32 = uri
        .query()
        .unwrap_or("")
        .split('&')
        .find_map(|v| v.strip_prefix("service_id="))
        .context("长连接缺少 service_id")?
        .parse()?;
    let mut interval = endpoint["data"]["ClientConfig"]["PingInterval"]
        .as_u64()
        .unwrap_or(120)
        .max(1);
    let mut ping = None::<Instant>;
    let mut received = Instant::now();
    let mut fragments = BTreeMap::<String, Parts>::new();
    {
        let mut health = health.lock().unwrap();
        health.connected = true;
        health.error = None;
    }
    while !stop.load(Ordering::Relaxed) {
        ensure!(
            received.elapsed() < Duration::from_secs(interval.saturating_mul(3).max(30)),
            "飞书心跳超时，重新连接"
        );
        if ping.is_none_or(|at| at.elapsed() >= Duration::from_secs(interval)) {
            let frame = Frame {
                service,
                headers: vec![Header {
                    key: "type".into(),
                    value: "ping".into(),
                }],
                ..Default::default()
            };
            ws.send(Message::Binary(frame.encode_to_vec().into()))?;
            ping = Some(Instant::now());
        }
        let message = match ws.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => anyhow::bail!("飞书长连接已断开，等待重新连接"),
        };
        received = Instant::now();
        let Message::Binary(bytes) = message else {
            continue;
        };
        let mut frame = Frame::decode(bytes)?;
        if frame.method == 0 {
            if frame.header("type")? == "pong"
                && let Some(payload) = frame.payload.as_ref().filter(|p| !p.is_empty())
            {
                let settings: Value = serde_json::from_slice(payload)?;
                if let Some(next) = settings["PingInterval"].as_u64() {
                    interval = next.max(1);
                }
            }
            continue;
        }
        if frame.method != 1 || frame.header("type")? != "event" {
            continue;
        }
        let sum: usize = frame.header("sum")?.parse()?;
        let seq: usize = frame.header("seq")?.parse()?;
        ensure!((1..=128).contains(&sum) && seq < sum, "飞书分片序号无效");
        let mut payload = frame.payload.clone().context("飞书帧缺少载荷")?;
        if sum > 1 {
            fragments.retain(|_, parts| parts.at.elapsed() < Duration::from_secs(30));
            let id = frame.header("message_id")?.to_owned();
            ensure!(
                fragments.len() < 64 || fragments.contains_key(&id),
                "飞书待合并消息过多"
            );
            let entry = fragments.entry(id.clone()).or_insert_with(|| Parts {
                at: Instant::now(),
                parts: vec![None; sum],
            });
            ensure!(entry.parts.len() == sum, "飞书分片数量变化");
            entry.parts[seq] = Some(payload);
            ensure!(
                entry.parts.iter().flatten().map(Vec::len).sum::<usize>() <= 1024 * 1024,
                "飞书事件过大"
            );
            if entry.parts.iter().any(Option::is_none) {
                continue;
            }
            payload = fragments
                .remove(&id)
                .unwrap()
                .parts
                .into_iter()
                .flatten()
                .flatten()
                .collect();
        }
        let result = (|| -> Result<()> {
            let event: Value = serde_json::from_slice(&payload)?;
            inbox.lock().unwrap().receive(config, &event)
        })();
        if let Err(error) = &result {
            health.lock().unwrap().error = Some(format!("飞书接收失败：{error:#}"));
        }
        frame.payload = Some(serde_json::to_vec(
            &json!({"code":if result.is_ok(){200}else{500}}),
        )?);
        ws.send(Message::Binary(frame.encode_to_vec().into()))?;
    }
    let _ = ws.close(None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_panic_is_visible_even_when_health_lock_was_poisoned() {
        for name in ["接收", "发送"] {
            let health = Arc::new(Mutex::new(Health {
                connected: true,
                ..Health::default()
            }));
            let inner = health.clone();
            let worker = spawn_worker(
                name,
                Arc::new(AtomicBool::new(false)),
                health.clone(),
                move || {
                    let _locked = inner.lock().unwrap();
                    panic!("模拟飞书线程故障");
                },
            );
            assert!(worker.join().is_err());
            let health = health.lock().unwrap();
            assert!(health.worker_errors[name].contains("异常退出"));
            if name == "接收" {
                assert!(!health.connected);
                assert!(health.error.is_some());
            }
        }
    }
    #[test]
    fn orderly_worker_exit_does_not_report_failure() {
        let health = Arc::new(Mutex::new(Health::default()));
        spawn_worker(
            "接收",
            Arc::new(AtomicBool::new(true)),
            health.clone(),
            || {},
        )
        .join()
        .unwrap();
        assert!(health.lock().unwrap().worker_errors.is_empty());
    }
    #[test]
    fn receives_only_allowed_private_user_text_and_deduplicates_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("feishu.json");
        let config = Config {
            app: "app".into(),
            secret: "secret".into(),
            users: HashSet::from(["ou_user".into()]),
            base: "http://127.0.0.1:1".into(),
        };
        let event = json!({"header":{"event_type":"im.message.receive_v1"},"event":{"sender":{"sender_type":"user","sender_id":{"open_id":"ou_user"}},"message":{"message_id":"om_message","chat_id":"oc_chat","chat_type":"p2p","message_type":"text","content":"{\"text\":\"执行任务\"}"}}});
        let mut inbox = Inbox::open(file.clone()).unwrap();
        inbox.receive(&config, &event).unwrap();
        let delivery = inbox.messages["om_message"].delivery.clone();
        let mut inbox = Inbox::open(file).unwrap();
        inbox.receive(&config, &event).unwrap();
        assert_eq!(inbox.messages.len(), 1);
        assert_eq!(inbox.messages["om_message"].delivery, delivery);
        for (field, value) in [("chat_type", "group"), ("message_type", "image")] {
            let mut changed = event.clone();
            changed["event"]["message"]["message_id"] = json!("other");
            changed["event"]["message"][field] = json!(value);
            inbox.receive(&config, &changed).unwrap();
            assert_eq!(inbox.messages.len(), 1);
        }
        let mut changed = event;
        changed["event"]["message"]["message_id"] = json!("other");
        changed["event"]["sender"]["sender_id"]["open_id"] = json!("stranger");
        inbox.receive(&config, &changed).unwrap();
        assert_eq!(inbox.messages.len(), 1);
    }
    #[test]
    fn frame_preserves_required_zero_fields_and_reply_headers() {
        let frame = Frame {
            headers: vec![Header {
                key: "type".into(),
                value: "ping".into(),
            }],
            ..Default::default()
        };
        let bytes = frame.encode_to_vec();
        assert!(bytes.starts_with(&[8, 0, 16, 0, 24, 0, 32, 0]));
        assert_eq!(Frame::decode(bytes.as_slice()).unwrap(), frame);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read},
        net::TcpListener,
    };
    trait ReadWrite: Read + Write + std::fmt::Debug {}
    impl<T: Read + Write + std::fmt::Debug> ReadWrite for T {}
    pub(super) fn read_http(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request = line.clone();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        format!("{request}{}", String::from_utf8(body).unwrap())
    }
    pub(super) fn respond(stream: &mut TcpStream, value: Value) {
        let body = value.to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    }
    fn config(port: u16) -> Config {
        Config {
            app: "test-app".into(),
            secret: "test-secret".into(),
            users: HashSet::from(["ou_allowed".into()]),
            base: format!("http://127.0.0.1:{port}"),
        }
    }
    #[test]
    fn websocket_ack_follows_durable_storage_and_duplicate_keeps_delivery_id() {
        check_websocket_delivery(false);
    }
    #[test]
    fn secure_websocket_handshake_receives_and_acknowledges_durable_events() {
        check_websocket_delivery(true);
    }
    fn certificate() -> (Arc<rustls::ServerConfig>, rustls::RootCertStore) {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
        )
        .unwrap();
        (Arc::new(server), roots)
    }
    #[test]
    fn tls_rejects_untrusted_certificates_and_wrong_hostnames() {
        for wrong_host in [false, true] {
            let (server_config, roots) = certificate();
            let tls = tls_client(if wrong_host {
                roots
            } else {
                rustls::RootCertStore::empty()
            })
            .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server_config).unwrap(),
                    socket,
                );
                assert!(tungstenite::accept(stream).is_err());
            });
            let socket = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let url = format!(
                "wss://{}:{}/",
                if wrong_host { "localhost" } else { "127.0.0.1" },
                address.port()
            );
            assert!(
                tungstenite::client_tls_with_config(
                    url,
                    socket,
                    None,
                    Some(Connector::Rustls(tls))
                )
                .is_err()
            );
            server.join().unwrap();
        }
    }
    fn check_websocket_delivery(secure: bool) {
        let temp = tempfile::tempdir().unwrap();
        let inbox = Arc::new(Mutex::new(
            Inbox::open(temp.path().join("inbox.json")).unwrap(),
        ));
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let ws = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = config(http.local_addr().unwrap().port());
        let (server_tls, roots) = certificate();
        let tls = tls_client(roots).unwrap();
        let url = format!(
            "{}://127.0.0.1:{}/events?service_id=7",
            if secure { "wss" } else { "ws" },
            ws.local_addr().unwrap().port()
        );
        let endpoint = thread::spawn(move || {
            let (mut stream, _) = http.accept().unwrap();
            assert!(read_http(&mut stream).contains("/callback/ws/endpoint"));
            respond(
                &mut stream,
                json!({"code":0,"data":{"URL":url,"ClientConfig":{"PingInterval":1}}}),
            );
        });
        let stop = Arc::new(AtomicBool::new(false));
        let health = Arc::new(Mutex::new(Health::default()));
        let receiver = {
            let inbox = inbox.clone();
            let stop = stop.clone();
            let health = health.clone();
            thread::spawn(move || receive_loop(&config, &inbox, &stop, &health, &tls))
        };
        let (socket, _) = ws.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let stream: Box<dyn ReadWrite> = if secure {
            Box::new(rustls::StreamOwned::new(
                rustls::ServerConnection::new(server_tls).unwrap(),
                socket,
            ))
        } else {
            Box::new(socket)
        };
        let mut ws = tungstenite::accept(stream).unwrap();
        let event = json!({"header":{"event_type":"im.message.receive_v1"},"event":{"sender":{"sender_type":"user","sender_id":{"open_id":"ou_allowed"}},"message":{"message_id":"om_once","chat_id":"oc_chat","chat_type":"p2p","message_type":"text","content":"{\"text\":\"任务\"}"}}});
        let frame = Frame {
            method: 1,
            service: 7,
            headers: vec![
                Header {
                    key: "type".into(),
                    value: "event".into(),
                },
                Header {
                    key: "sum".into(),
                    value: "1".into(),
                },
                Header {
                    key: "seq".into(),
                    value: "0".into(),
                },
            ],
            payload: Some(serde_json::to_vec(&event).unwrap()),
            ..Default::default()
        };
        let mut delivery = None;
        for _ in 0..2 {
            ws.send(Message::Binary(frame.encode_to_vec().into()))
                .unwrap();
            loop {
                let Message::Binary(bytes) = ws.read().unwrap() else {
                    continue;
                };
                let frame = Frame::decode(bytes).unwrap();
                if frame.method == 0 {
                    continue;
                }
                assert_eq!(
                    serde_json::from_slice::<Value>(&frame.payload.unwrap()).unwrap()["code"],
                    200
                );
                break;
            }
            let saved = Inbox::open(temp.path().join("inbox.json")).unwrap();
            assert_eq!(saved.messages.len(), 1);
            let id = &saved.messages["om_once"].delivery;
            if let Some(previous) = &delivery {
                assert_eq!(previous, id);
            } else {
                delivery = Some(id.clone());
            }
        }
        stop.store(true, Ordering::Relaxed);
        receiver.join().unwrap().unwrap();
        endpoint.join().unwrap();
    }
    #[test]
    fn unknown_reply_is_persisted_and_never_automatically_sent_again() {
        let temp = tempfile::tempdir().unwrap();
        let http = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = config(http.local_addr().unwrap().port());
        let server = thread::spawn(move || {
            let (mut stream, _) = http.accept().unwrap();
            assert!(read_http(&mut stream).contains("tenant_access_token"));
            respond(
                &mut stream,
                json!({"code":0,"tenant_access_token":"fake-token"}),
            );
            let (mut stream, _) = http.accept().unwrap();
            assert!(read_http(&mut stream).contains("/om_message/reply"));
            // 请求已经被服务接收，但回复丢失；不能自动再发送。
        });
        let file = temp.path().join("feishu.json");
        let mut inbox = Inbox::open(file.clone()).unwrap();
        inbox.messages.insert(
            "om_message".into(),
            Incoming {
                message: "om_message".into(),
                user: "ou_allowed".into(),
                chat: "oc_chat".into(),
                text: "request".into(),
                delivery: "delivery".into(),
                thread: Some("thread".into()),
                replies: vec![Reply {
                    text: "result".into(),
                    status: "queued".into(),
                }],
                finished: true,
                error: None,
            },
        );
        inbox.save().unwrap();
        let inbox = Mutex::new(inbox);
        assert!(
            dispatch(
                &temp.path().join("config.toml"),
                &config,
                &inbox,
                "om_message"
            )
            .is_err()
        );
        server.join().unwrap();
        let saved = Mutex::new(Inbox::open(file).unwrap());
        assert_eq!(
            saved.lock().unwrap().messages["om_message"].replies[0].status,
            "sending"
        );
        dispatch(
            &temp.path().join("config.toml"),
            &config,
            &saved,
            "om_message",
        )
        .unwrap();
    }
}
