//! 已存在的本机 app-server，只读查询；不启动 daemon、不恢复线程。
use crate::{console::query, git::checked};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::ErrorKind,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};

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

pub fn thread_not_loaded(error: &anyhow::Error, method: &str, id: &str) -> bool {
    error.downcast_ref::<RpcError>().is_some_and(|error| {
        error.method == method
            && error.value["code"] == -32600
            && error.value["message"] == format!("thread not loaded: {id}")
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub cwd: PathBuf,
    pub status: Value,
    #[serde(default)]
    pub ephemeral: bool,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub source: Value,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub created_at: i64,
}
impl Thread {
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.preview)
    }
    pub fn label(&self) -> String {
        status_label(&self.status)
    }
    pub fn priority(&self) -> u8 {
        match self.status["type"].as_str() {
            Some("active")
                if self.status["activeFlags"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty()) =>
            {
                0
            }
            Some("systemError") => 1,
            Some("active") => 2,
            Some("idle") => 3,
            _ => 4,
        }
    }
}
pub fn status_label(status: &Value) -> String {
    match status["type"].as_str() {
        Some("active") => {
            let Some(flags) = status["activeFlags"].as_array() else {
                return "未知：active 缺少标记".into();
            };
            if flags.is_empty() {
                return "运行中".into();
            }
            flags
                .iter()
                .map(|flag| match flag.as_str() {
                    Some("waitingOnApproval") => "等待审批".to_owned(),
                    Some("waitingOnUserInput") => "等待输入".to_owned(),
                    _ => format!("未知标记 {flag}"),
                })
                .collect::<Vec<_>>()
                .join(" / ")
        }
        Some("idle") => "空闲".into(),
        Some("notLoaded") => "当前服务未加载".into(),
        Some("systemError") => "会话错误".into(),
        _ => format!("未知状态 {status}"),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub data: Vec<Thread>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Turn {
    pub id: String,
    pub status: String,
}

pub struct Client {
    socket: WebSocket<UnixStream>,
    next_id: u64,
}
impl Client {
    pub fn discover() -> Result<Self> {
        let text = checked(
            "codex",
            &["app-server".as_ref(), "daemon".as_ref(), "version".as_ref()],
            None,
        )
        .context("无法发现 Codex 服务（需要支持 daemon version 的 Codex CLI）")?;
        let info: Value = serde_json::from_str(&text).context("Codex 服务信息不是 JSON")?;
        ensure!(
            info["status"] == "running",
            "Codex 服务未运行；Console 不自动启动服务"
        );
        let path = info["socketPath"]
            .as_str()
            .context("Codex 未返回 socketPath")?;
        Self::connect(Path::new(path))
    }
    pub fn connect(path: &Path) -> Result<Self> {
        query::check()?;
        let socket = UnixStream::connect(path)
            .with_context(|| format!("无法连接 Codex socket：{}", path.display()))?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        let config = WebSocketConfig::default().max_message_size(Some(16 * 1024 * 1024));
        let (socket, _) =
            tungstenite::client::client_with_config("ws://localhost/", socket, Some(config))
                .map_err(|error| anyhow::anyhow!("Codex WebSocket 握手失败：{error}"))?;
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut client = Self { socket, next_id: 0 };
        client.request("initialize", json!({"clientInfo":{"name":"twig_console", "version":env!("CARGO_PKG_VERSION")}, "capabilities":{"experimentalApi":true}}))?;
        client.socket.send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))?;
        Ok(client)
    }
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        query::check()?;
        self.next_id += 1;
        let id = self.next_id;
        self.socket.send(Message::Text(
            json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))?;
        let start = Instant::now();
        loop {
            query::check()?;
            ensure!(
                start.elapsed() < Duration::from_secs(2),
                "Codex {method} 超过 2 秒"
            );
            let message = match self.socket.read() {
                Err(tungstenite::Error::Io(ref error))
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    continue;
                }
                other => other.with_context(|| format!("Codex {method} 连接中断"))?,
            };
            match message {
                Message::Text(text) => {
                    let mut value: Value = serde_json::from_str(text.as_str())?;
                    // 观察者不回复审批请求，不把通知误认为查询结果。
                    if value["id"].as_u64() != Some(id) || value.get("method").is_some() {
                        continue;
                    }
                    if let Some(error) = value.get("error") {
                        // 新线程在首条用户消息前尚无轮次存储，协议以此错误表示空历史。
                        if method == "thread/turns/list"
                            && error["code"] == -32600
                            && params["threadId"].as_str().is_some_and(|thread| {
                                error["message"] == format!("thread {thread} is not materialized yet; thread/turns/list is unavailable before first user message")
                            })
                        {
                            return Ok(json!({"data":[],"nextCursor":null}));
                        }
                        return Err(RpcError {
                            method: method.to_owned(),
                            value: error.clone(),
                        }
                        .into());
                    }
                    return value
                        .get_mut("result")
                        .map(Value::take)
                        .context("Codex 响应缺少 result");
                }
                Message::Close(_) => bail!("Codex 服务关闭连接"),
                Message::Ping(_) => {
                    self.socket.flush()?;
                }
                _ => {}
            }
        }
    }
    pub fn loaded(&mut self) -> Result<Vec<Thread>> {
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut ids = HashSet::new();
        let mut threads = Vec::new();
        loop {
            let page = self.request("thread/loaded/list", json!({"limit":100,"cursor":cursor}))?;
            for id in page["data"].as_array().context("loaded/list 缺少 data")? {
                let id = id.as_str().context("loaded/list ID 无效")?;
                if !ids.insert(id.to_owned()) {
                    continue;
                }
                let mut result = match self
                    .request("thread/read", json!({"threadId":id,"includeTurns":false}))
                {
                    Ok(result) => result,
                    Err(error) if thread_not_loaded(&error, "thread/read", id) => continue,
                    Err(error) => return Err(error),
                };
                let thread: Thread = serde_json::from_value(result["thread"].take())
                    .context("Codex 线程格式不兼容")?;
                if !thread.ephemeral {
                    threads.push(thread);
                }
            }
            cursor = serde_json::from_value(page["nextCursor"].clone())?;
            match &cursor {
                Some(c) => ensure!(cursors.insert(c.clone()), "Codex 返回重复分页游标"),
                None => break,
            }
        }
        Ok(threads)
    }
    pub fn history(&mut self, cwd: Option<&Path>, cursor: Option<&str>) -> Result<Page> {
        let result = self.request("thread/list", json!({"cwd":cwd,"cursor":cursor,"limit":50,"useStateDbOnly":true,"sortKey":"updated_at","sortDirection":"desc"}))?;
        serde_json::from_value(result).context("Codex 历史列表格式不兼容")
    }
    pub fn latest_turn(&mut self, id: &str) -> Result<String> {
        let result = match self.request(
            "thread/turns/list",
            json!({"threadId":id,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"}),
        ) {
            Ok(result) => result,
            Err(error) if thread_not_loaded(&error, "thread/turns/list", id) => {
                return Ok("当前服务未加载此会话，轮次状态暂不可用".into());
            }
            Err(error) => return Err(error),
        };
        let turns = result["data"]
            .as_array()
            .context("Codex 轮次列表缺少 data")?;
        let Some(turn) = turns.first() else {
            return Ok("尚无轮次记录".into());
        };
        let status = match turn["status"].as_str() {
            Some("completed") => "本轮已完成",
            Some("failed") => "本轮失败",
            Some("interrupted") => "本轮中断",
            Some("inProgress") => "本轮进行中",
            _ => "未知轮次状态",
        };
        Ok(format!(
            "{}\n开始：{}  完成：{}\n错误：{}",
            status, turn["startedAt"], turn["completedAt"], turn["error"]
        ))
    }

    /// 首次只建立基线；之后读到上次最新轮次（包含它，以捕获进行中→完成）。
    /// 整次查询成功后调用方才更新基线，分页失败不会丢失通知。
    pub fn completed_turns(
        &mut self,
        id: &str,
        previous: Option<&Option<Turn>>,
    ) -> Result<(Option<Turn>, Vec<Turn>)> {
        completed_turns(id, previous, |method, params| self.request(method, params))
    }
}

pub(crate) fn completed_turns(
    id: &str,
    previous: Option<&Option<Turn>>,
    mut request: impl FnMut(&str, Value) -> Result<Value>,
) -> Result<(Option<Turn>, Vec<Turn>)> {
    let anchor = previous.and_then(Option::as_ref);
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut newest = None;
    let mut completed = Vec::new();
    loop {
        let result = request(
            "thread/turns/list",
            json!({"threadId":id,"limit":if previous.is_none() {1} else {50},
                    "cursor":cursor,"sortDirection":"desc","itemsView":"notLoaded"}),
        )?;
        let turns: Vec<Turn> =
            serde_json::from_value(result["data"].clone()).context("Codex 轮次格式不兼容")?;
        for turn in turns {
            if newest.is_none() {
                newest = Some(turn.clone());
            }
            let at_anchor = anchor.is_some_and(|old| old.id == turn.id);
            if previous.is_some()
                && turn.status == "completed"
                && (!at_anchor || anchor.is_some_and(|old| old.status != "completed"))
            {
                completed.push(turn);
            }
            if at_anchor {
                completed.reverse();
                return Ok((newest, completed));
            }
        }
        if previous.is_none() {
            return Ok((newest, completed));
        }
        cursor = serde_json::from_value(result["nextCursor"].clone())?;
        match &cursor {
            Some(c) => ensure!(cursors.insert(c.clone()), "Codex 返回重复分页游标"),
            None => {
                // 历史被回退/裁剪后，不能将找不到锚点的旧轮次当作新完成。
                if anchor.is_some() {
                    completed.clear();
                }
                completed.reverse();
                return Ok((newest, completed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixListener,
        sync::{Arc, Mutex},
        thread,
    };
    fn mock_thread(id: &str, status: Value) -> Value {
        json!({"id":id,"cwd":"/tmp","status":status,"source":"cli","name":"测试会话"})
    }
    #[test]
    fn unloaded_threads_preserve_progress_and_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut polls = 0;
            while let Ok(Message::Text(text)) = ws.read() {
                let v: Value = serde_json::from_str(&text).unwrap();
                let method = v["method"].as_str().unwrap();
                let id = v["params"]["threadId"].as_str().unwrap_or("");
                let result = match method {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/loaded/list" => json!({"data":["gone", "healthy"]}),
                    "thread/read" if id == "healthy" => {
                        json!({"thread":mock_thread(id, json!({"type":"idle"}))})
                    }
                    "thread/read" => {
                        json!({"error":{"code":-32600,"message":"thread not loaded: gone"}})
                    }
                    "thread/turns/list" => {
                        polls += 1;
                        match polls {
                            1 => json!({"data":[{"id":"run","status":"inProgress"}]}),
                            2 | 3 => {
                                json!({"error":{"code":-32600,"message":"thread not loaded: healthy"}})
                            }
                            _ => json!({"data":[{"id":"run","status":"completed"}]}),
                        }
                    }
                    _ => panic!("不允许的请求：{method}"),
                };
                let response = match result.get("error") {
                    Some(error) => json!({"id":v["id"],"error":error}),
                    None => json!({"id":v["id"],"result":result}),
                };
                ws.send(Message::Text(response.to_string().into())).unwrap();
            }
            assert_eq!(polls, 5);
        });
        let mut client = Client::connect(&path).unwrap();
        let threads = client.loaded().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "healthy");
        let (running, _) = client.completed_turns("healthy", None).unwrap();
        let error = client
            .completed_turns("healthy", Some(&running))
            .unwrap_err();
        assert!(thread_not_loaded(&error, "thread/turns/list", "healthy"));
        assert!(!thread_not_loaded(&error, "thread/turns/list", "other"));
        assert!(!thread_not_loaded(&error, "thread/read", "healthy"));
        assert!(client.latest_turn("healthy").unwrap().contains("未加载"));
        let (done, events) = client.completed_turns("healthy", Some(&running)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "run");
        assert!(
            client
                .completed_turns("healthy", Some(&done))
                .unwrap()
                .1
                .is_empty()
        );
        let wrong_code = RpcError {
            method: "thread/turns/list".into(),
            value: json!({"code":-32603,"message":"thread not loaded: healthy"}),
        };
        assert!(!thread_not_loaded(
            &wrong_code.into(),
            "thread/turns/list",
            "healthy"
        ));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn completion_baseline_pagination_and_retry_are_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let turn = |id: &str, status: &str| json!({"id":id,"status":status});
        let responses = vec![
            json!({"data":[turn("old","completed")]}),
            json!({"data":[turn("run","inProgress"),turn("old","completed")]}),
            json!({"data":[turn("run","completed")]}),
            json!({"data":[turn("run","completed")]}),
            json!({"data":[turn("new","completed"),turn("failed","failed")],"nextCursor":"page2"}),
            json!({"error":{"message":"分页失败"}}),
            json!({"data":[turn("new","completed"),turn("failed","failed")],"nextCursor":"page2"}),
            json!({"data":[turn("middle","completed"),turn("stop","interrupted"),turn("run","completed")]}),
            json!({"data":[turn("historic","completed")]}),
        ];
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut responses = responses.into_iter();
            while let Ok(Message::Text(text)) = ws.read() {
                let v: Value = serde_json::from_str(&text).unwrap();
                let result = match v["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/turns/list" => {
                        assert_eq!(v["params"]["itemsView"], "notLoaded");
                        responses.next().unwrap()
                    }
                    other => panic!("不允许的请求：{other}"),
                };
                let response = if let Some(error) = result.get("error") {
                    json!({"id":v["id"],"error":error})
                } else {
                    json!({"id":v["id"],"result":result})
                };
                ws.send(Message::Text(response.to_string().into())).unwrap();
            }
            assert!(responses.next().is_none());
        });
        let mut client = Client::connect(&path).unwrap();
        let (old, events) = client.completed_turns("thread", None).unwrap();
        assert!(events.is_empty());
        let (running, events) = client.completed_turns("thread", Some(&old)).unwrap();
        assert!(events.is_empty());
        let (done, events) = client.completed_turns("thread", Some(&running)).unwrap();
        assert_eq!(events[0].id, "run");
        assert!(
            client
                .completed_turns("thread", Some(&done))
                .unwrap()
                .1
                .is_empty()
        );
        assert!(client.completed_turns("thread", Some(&done)).is_err());
        let (newest, events) = client.completed_turns("thread", Some(&done)).unwrap();
        assert_eq!(
            events.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["middle", "new"]
        );
        assert!(
            client
                .completed_turns("thread", Some(&newest))
                .unwrap()
                .1
                .is_empty()
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn unmaterialized_thread_stays_empty_until_first_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut calls = 0;
            while let Ok(Message::Text(text)) = ws.read() {
                let v: Value = serde_json::from_str(&text).unwrap();
                let response = match v["method"].as_str().unwrap() {
                    "initialize" => json!({"id":v["id"],"result":{}}),
                    "initialized" => continue,
                    "thread/turns/list" => {
                        calls += 1;
                        if calls <= 5 {
                            let thread = if calls == 4 { "another" } else { "thread" };
                            let code = if calls == 5 { -32603 } else { -32600 };
                            json!({"id":v["id"],"error":{"code":code,"message":format!(
                                "thread {thread} is not materialized yet; thread/turns/list is unavailable before first user message"
                            )}})
                        } else {
                            json!({"id":v["id"],"result":{"data":[{"id":"first","status":"completed"}]}})
                        }
                    }
                    other => panic!("不允许的请求：{other}"),
                };
                ws.send(Message::Text(response.to_string().into())).unwrap();
            }
            assert_eq!(calls, 7);
        });
        let mut client = Client::connect(&path).unwrap();
        let (empty, events) = client.completed_turns("thread", None).unwrap();
        assert!(empty.is_none() && events.is_empty());
        let (empty, events) = client.completed_turns("thread", Some(&empty)).unwrap();
        assert!(empty.is_none() && events.is_empty());
        assert_eq!(client.latest_turn("thread").unwrap(), "尚无轮次记录");
        // 其他线程或错误码不能被当作空历史，重试仍沿用之前的基线。
        assert!(client.completed_turns("thread", Some(&empty)).is_err());
        assert!(client.completed_turns("thread", Some(&empty)).is_err());
        let (latest, events) = client.completed_turns("thread", Some(&empty)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "first");
        assert!(
            client
                .completed_turns("thread", Some(&latest))
                .unwrap()
                .1
                .is_empty()
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn initially_empty_thread_can_complete_between_polls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let mut calls = 0;
            while let Ok(Message::Text(text)) = ws.read() {
                let v: Value = serde_json::from_str(&text).unwrap();
                let result = match v["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/turns/list" => {
                        calls += 1;
                        if calls == 1 {
                            json!({"data":[]})
                        } else {
                            json!({"data":[{"id":"fast","status":"completed"}]})
                        }
                    }
                    other => panic!("不允许的请求：{other}"),
                };
                ws.send(Message::Text(
                    json!({"id":v["id"],"result":result}).to_string().into(),
                ))
                .unwrap();
            }
        });
        let mut client = Client::connect(&path).unwrap();
        let (empty, _) = client.completed_turns("thread", None).unwrap();
        assert!(empty.is_none());
        let (_, events) = client.completed_turns("thread", Some(&empty)).unwrap();
        assert_eq!(events[0].id, "fast");
        drop(client);
        server.join().unwrap();
    }
    #[test]
    fn reads_pages_status_and_turns_without_mutating_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let methods = Arc::new(Mutex::new(Vec::new()));
        let logged = methods.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            while let Ok(Message::Text(text)) = ws.read() {
                let v: Value = serde_json::from_str(text.as_str()).unwrap();
                let method = v["method"].as_str().unwrap();
                logged.lock().unwrap().push(method.to_owned());
                let result = match method {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/loaded/list" => {
                        if v["params"]["cursor"].is_null() {
                            json!({"data":["a"],"nextCursor":"page2"})
                        } else {
                            json!({"data":["a","temporary","b"],"nextCursor":null})
                        }
                    }
                    "thread/read" => {
                        assert_eq!(v["params"]["includeTurns"], false);
                        let id = v["params"]["threadId"].as_str().unwrap();
                        let mut thread = mock_thread(
                            id,
                            json!({"type":"active","activeFlags":["waitingOnApproval","waitingOnUserInput"]}),
                        );
                        thread["ephemeral"] = json!(id == "temporary");
                        json!({"thread":thread})
                    }
                    "thread/list" => {
                        assert_eq!(v["params"]["useStateDbOnly"], true);
                        json!({"data":[mock_thread("old",json!({"type":"notLoaded"}))],"nextCursor":"older"})
                    }
                    "thread/turns/list" => {
                        assert_eq!(v["params"]["itemsView"], "notLoaded");
                        json!({"data":[{"id":"turn-old","status":"completed","startedAt":1,"completedAt":2,"error":null}]})
                    }
                    _ => panic!("不允许的写操作：{method}"),
                };
                ws.send(Message::Text(
                    json!({"method":"thread/status/changed","params":{"threadId":"other"}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
                ws.send(Message::Text(
                    json!({"id":v["id"],"result":result}).to_string().into(),
                ))
                .unwrap();
            }
        });
        let mut c = Client::connect(&path).unwrap();
        let threads = c.loaded().unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(
            threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(threads[0].label(), "等待审批 / 等待输入");
        assert_eq!(
            c.history(Some(Path::new("/tmp")), None)
                .unwrap()
                .next_cursor
                .as_deref(),
            Some("older")
        );
        assert!(c.latest_turn("a").unwrap().starts_with("本轮已完成"));
        drop(c);
        server.join().unwrap();
        assert_eq!(
            methods
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == "thread/read")
                .count(),
            3
        );
    }
    #[test]
    fn rejects_closed_and_stalled_servers_and_can_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.read().unwrap();
            ws.close(None).unwrap();
            drop(ws);
            let (stream, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            ws.read().unwrap();
            thread::sleep(Duration::from_millis(2300));
        });
        assert!(Client::connect(&path).is_err());
        let start = Instant::now();
        assert!(Client::connect(&path).is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        server.join().unwrap();
    }
    #[test]
    fn status_never_confuses_idle_unloaded_or_unknown_with_completion() {
        assert_eq!(status_label(&json!({"type":"idle"})), "空闲");
        assert_eq!(status_label(&json!({"type":"notLoaded"})), "当前服务未加载");
        assert!(status_label(&json!({"type":"future"})).contains("未知"));
        assert!(status_label(&json!({"type":"active","activeFlags":["future"]})).contains("未知"));
    }
}
