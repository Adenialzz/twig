//! 主控写接口独立于 Console 的只读 Codex 客户端。
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};

#[derive(Debug)]
struct RpcError {
    method: String,
    value: Value,
}
impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Codex {}：{}", self.method, self.value)
    }
}
impl std::error::Error for RpcError {}

fn matches_error(error: &anyhow::Error, method: &str, message: &str) -> bool {
    error.downcast_ref::<RpcError>().is_some_and(|e| {
        e.method == method && e.value["code"] == -32600 && e.value["message"] == message
    })
}
pub fn thread_not_loaded(error: &anyhow::Error, id: &str) -> bool {
    matches_error(error, "thread/read", &format!("thread not loaded: {id}"))
}
pub fn no_rollout(error: &anyhow::Error, id: &str) -> bool {
    matches_error(
        error,
        "thread/resume",
        &format!("no rollout found for thread id {id}"),
    )
}

pub struct Client {
    socket: WebSocket<UnixStream>,
    next_id: u64,
    pub requests: Vec<Value>,
}

pub fn discover() -> Result<PathBuf> {
    let output = crate::git::checked(
        "codex",
        &["app-server".as_ref(), "daemon".as_ref(), "version".as_ref()],
        None,
    )
    .context("无法发现 Codex daemon；请安装 Codex 并自行运行 codex app-server daemon start")?;
    let info: Value = serde_json::from_str(&output)?;
    ensure!(
        info["status"] == "running",
        "Codex daemon 未运行；请自行运行 codex app-server daemon start，再重试 Agent"
    );
    Ok(PathBuf::from(
        info["socketPath"]
            .as_str()
            .context("Codex 未返回 socketPath")?,
    ))
}

impl Client {
    pub fn thread(&mut self, id: &str) -> Result<Value> {
        let metadata = self.call("thread/read", json!({"threadId":id,"includeTurns":false}))?;
        // 新建且尚无输入的 session 没有可分页的轮次存储。
        if metadata["thread"]["preview"]
            .as_str()
            .is_none_or(str::is_empty)
        {
            return Ok(metadata);
        }
        self.call("thread/read", json!({"threadId":id,"includeTurns":true}))
    }
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .context("无法连接 Codex daemon；请检查 codex app-server daemon version")?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let (socket, _) = tungstenite::client("ws://localhost/", stream)?;
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut client = Self {
            socket,
            next_id: 0,
            requests: Vec::new(),
        };
        client.call("initialize", json!({"clientInfo":{"name":"twig_chat","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}))?;
        client.send(json!({"method":"initialized"}))?;
        Ok(client)
    }
    fn send(&mut self, value: Value) -> Result<()> {
        self.socket.send(Message::Text(value.to_string().into()))?;
        Ok(())
    }
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            ensure!(
                Instant::now() < deadline,
                "Codex {method} 超时；写请求结果可能未知，请核对会话，勿直接重发"
            );
            let message = match self.socket.read() {
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                result => result.context("Codex 连接中断；写请求结果可能未知")?,
            };
            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)?;
                    if value.get("method").is_some() {
                        if value["method"] == "serverRequest/resolved" {
                            self.requests
                                .retain(|request| request["id"] != value["params"]["requestId"]);
                        }
                        if value.get("id").is_some() {
                            self.requests.push(value);
                        }
                        continue;
                    }
                    if value["id"] != id {
                        continue;
                    }
                    if let Some(error) = value.get("error") {
                        return Err(RpcError {
                            method: method.into(),
                            value: error.clone(),
                        }
                        .into());
                    }
                    return value
                        .get("result")
                        .cloned()
                        .context("Codex 响应缺少 result");
                }
                Message::Ping(_) => self.socket.flush()?,
                Message::Close(_) => {
                    return Err(tungstenite::Error::ConnectionClosed).context("Codex 关闭连接");
                }
                _ => {}
            }
        }
    }
    pub fn answer(&mut self, request: &Value, result: Value) -> Result<()> {
        self.send(json!({"id":request["id"],"result":result}))
    }
}
