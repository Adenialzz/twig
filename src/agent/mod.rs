//! 可选主控工作流；运行时由 Codex daemon 托管，Twig 不启动后台服务。
pub mod chat;
pub mod rpc;

use anyhow::{Context, Result, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Args)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Subcommand)]
pub enum Action {
    /// 初始化主控 session；不启动 Codex daemon
    Init,
    /// 查询主控、受管任务和消息投递状态（JSON）
    Status,
    /// 向主控或指定受管任务发消息；忙时排队
    Send {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        message: String,
    },
    /// 读取主控或指定任务的 Codex 历史（JSON）
    Read {
        #[command(flatten)]
        target: Target,
    },
    /// 核对未确认消息并尝试投递待发队列；不重发结果未知的消息
    Drain,
    /// 取消排队或待核对的投递记录；不会撤回已发给 Codex 的内容
    Cancel {
        #[arg(long)]
        id: String,
    },
    /// 通过飞书机器人发送重要通知；读取 TWIG_FEISHU_WEBHOOK_URL
    Notify {
        #[arg(long)]
        message: String,
    },
    /// Codex 完成回调；由受管 session 自动调用
    #[command(hide = true)]
    Event { event: String },
}
#[derive(Args, Default)]
pub struct Target {
    #[arg(long, requires = "task")]
    project: Option<String>,
    #[arg(long, requires = "project")]
    task: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub thread: String,
    pub cwd: PathBuf,
    pub project: Option<String>,
    pub task: Option<String>,
    pub stopped: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub id: String,
    pub thread: String,
    pub text: String,
    pub status: String,
    pub turn: Option<String>,
    pub error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub socket: Option<PathBuf>,
    pub bindings: BTreeMap<String, Binding>,
    pub messages: Vec<Delivery>,
    pub seen: Vec<String>,
}

pub struct Store {
    pub registry: PathBuf,
    pub dir: PathBuf,
}
struct Guard(File);
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
impl Store {
    pub fn new(registry: &Path) -> Result<Self> {
        let parent = registry.parent().context("登记表无父目录")?;
        // 同目录多个配置也不能共用主控记录；默认配置保留简短目录。
        let dir = if registry.file_name().is_some_and(|n| n == "config.toml") {
            parent.join("controller")
        } else {
            parent
                .join("controller")
                .join(crate::config::repository_id(registry))
        };
        Ok(Self {
            registry: registry.to_owned(),
            dir,
        })
    }
    fn lock(&self) -> Result<Guard> {
        fs::create_dir_all(&self.dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.dir.join("lock"))?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        ensure!(
            result == 0,
            "无法锁定主控记录：{}",
            std::io::Error::last_os_error()
        );
        Ok(Guard(file))
    }
    pub fn load(&self) -> Result<State> {
        match fs::read(self.dir.join("runtime.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).context("主控记录损坏")?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, state: &State) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)?;
        serde_json::to_writer_pretty(&mut file, state)?;
        file.as_file().sync_all()?;
        file.persist(self.dir.join("runtime.json"))?;
        Ok(())
    }
    fn client(&self, state: &State) -> Result<rpc::Client> {
        let path = rpc::discover()?;
        if let Some(expected) = &state.socket {
            ensure!(
                expected == &path,
                "Codex 服务路径已变化；请核对主控记录，不自动连接另一服务"
            );
        }
        rpc::Client::connect(&path)
    }
    pub fn binding(&self, target: &Target) -> Result<Binding> {
        let state = self.load()?;
        let id = if let (Some(alias), Some(name)) = (&target.project, &target.task) {
            let entry = crate::registry::entry(&self.registry, alias)?;
            let project = crate::config::Project::discover_at(
                &crate::registry::expand(&entry.path)?,
                &self.registry,
            )?;
            project
                .state()?
                .get(name)
                .and_then(|task| task.agent_thread.clone())
                .context("任务不是受管任务；仅支持通过 twig new --agent 新建的任务")?
        } else {
            "controller".into()
        };
        state
            .bindings
            .get(&id)
            .cloned()
            .context("尚未绑定会话；主控请运行 twig agent init，任务请通过 twig new --agent 新建")
    }
    pub fn validate(&self, binding: &Binding) -> Result<()> {
        if let (Some(alias), Some(name)) = (&binding.project, &binding.task) {
            let entry = crate::registry::entry(&self.registry, alias)?;
            let project = crate::config::Project::discover_at(
                &crate::registry::expand(&entry.path)?,
                &self.registry,
            )?;
            let state = project.state()?;
            let task = state.get(name).context("任务已取消登记；停止投递")?;
            ensure!(
                task.agent_thread.as_deref() == Some(&binding.thread) && task.path == binding.cwd,
                "任务实例已变化；停止投递旧消息"
            );
            ensure!(
                crate::check_worktree(&project.config()?, task)?,
                "任务 Worktree 不存在"
            );
        }
        Ok(())
    }
    fn callback(&self) -> Result<Vec<String>> {
        Ok(vec![
            std::env::current_exe()?.to_string_lossy().into(),
            "--config".into(),
            self.registry.to_string_lossy().into(),
            "agent".into(),
            "event".into(),
        ])
    }
    pub fn initialize(&self) -> Result<Binding> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        if let Some(binding) = state.bindings.get("controller") {
            let mut client = self.client(&state)?;
            match client.call(
                "thread/read",
                json!({"threadId":binding.thread,"includeTurns":false}),
            ) {
                Ok(result) => {
                    check_thread(binding, &result)?;
                    return Ok(binding.clone());
                }
                Err(error) if rpc::thread_not_loaded(&error, &binding.thread) => {
                    match client.call("thread/resume", json!({"threadId":binding.thread,"excludeTurns":true,"config":{"notify":self.callback()?}})) {
                        Ok(result) => {
                            check_thread(binding, &result)?;
                            return Ok(binding.clone());
                        }
                        Err(error) if rpc::no_rollout(&error, &binding.thread) => {
                            ensure!(
                                !state.messages.iter().any(|m| m.thread == binding.thread)
                                    && !state.seen.iter().any(|key| key.starts_with(&format!("{}:", binding.thread))),
                                "主控会话已丢失但存在消息或完成记录；保留绑定，请核对 Codex 历史，拒绝创建替代会话：{error}"
                            );
                            // 确认无 rollout 且从未投递过消息，才替换失效的空绑定。
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        let socket = rpc::discover()?;
        let mut client = rpc::Client::connect(&socket)?;
        let cwd = self.dir.join("workspace");
        fs::create_dir_all(&cwd)?;
        let exe = std::env::current_exe()?;
        let instructions = format!(
            "你是 Twig 主控 Agent，使用中文与用户沟通。工作目录中的 state.md 保存全局计划。通过 CLI 管理所有项目与任务。Twig 可执行文件：{}；每次命令必须带 --config {}。\n先阅读 twig guide agent 和 twig guide automation。查询项目使用 twig ls；派活使用 twig new PROJECT TASK --agent --no-open，然后 twig agent send --project PROJECT --task TASK --message 内容。用 twig agent read 查看任务结果。任务在各自 Worktree 工作，维护 .twig/state.md。任务完成回调会给你发消息；等待期间结束本轮，无需轮询或持续推理。轮次结束不等于目标完成，需对照验收标准和测试结果。需要用户决策或重要目标完成时调用 twig agent notify --message 内容，通过飞书提醒。不要把自己的完成事件当成新任务。遵循用户已授权的目标和项目约定。",
            exe.display(),
            self.registry.display()
        );
        write_new(&cwd.join("AGENTS.md"), &instructions)?;
        write_new(
            &cwd.join("state.md"),
            "# 全局计划\n\n## 目标\n待用户交代\n\n## 待跟进\n无\n",
        )?;
        let result = client.call("thread/start", json!({"cwd":cwd,"developerInstructions":instructions,"config":{"notify":self.callback()?}}))?;
        let binding = Binding {
            thread: result["thread"]["id"]
                .as_str()
                .context("缺少 thread ID")?
                .into(),
            cwd,
            project: None,
            task: None,
            stopped: false,
        };
        state.socket = Some(socket);
        state.bindings.insert("controller".into(), binding.clone());
        self.save(&state)?;
        Ok(binding)
    }
    pub fn create_task(&self, alias: &str, name: &str, cwd: &Path) -> Result<String> {
        self.initialize()?;
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let mut client = self.client(&state)?;
        let instructions = "你是 Twig 受管任务 Agent。请遵循项目约定，并维护 .twig/state.md 中的目标、进展、阻塞、下一步和验证结果；阶段完成或需要用户决定时先更新文件再结束本轮。主控通过会话消息派活和追问。完成一轮并不等于任务已验收。";
        let result = client.call("thread/start", json!({"cwd":cwd,"developerInstructions":instructions,"config":{"notify":self.callback()?}}))?;
        let thread = result["thread"]["id"]
            .as_str()
            .context("缺少 thread ID")?
            .to_owned();
        state.bindings.insert(
            thread.clone(),
            Binding {
                thread: thread.clone(),
                cwd: cwd.to_owned(),
                project: Some(alias.into()),
                task: Some(name.into()),
                stopped: false,
            },
        );
        self.save(&state)?;
        Ok(thread)
    }
    pub fn enqueue(&self, binding: &Binding, text: String) -> Result<String> {
        ensure!(!text.trim().is_empty(), "消息不能为空");
        self.validate(binding)?;
        let _lock = self.lock()?;
        let mut state = self.load()?;
        ensure!(
            !state
                .bindings
                .values()
                .any(|b| b.thread == binding.thread && b.stopped),
            "任务已停止；先通过 twig open 显式重新进入"
        );
        ensure!(
            state.bindings.values().any(|b| b.thread == binding.thread),
            "会话绑定已变化"
        );
        let id = message_id();
        state.messages.push(Delivery {
            id: id.clone(),
            thread: binding.thread.clone(),
            text,
            status: "queued".into(),
            turn: None,
            error: None,
        });
        self.save(&state)?;
        drop(_lock);
        if let Err(error) = self.flush() {
            let _lock = self.lock()?;
            let mut state = self.load()?;
            if let Some(message) = state
                .messages
                .iter_mut()
                .find(|message| message.id == id && message.status == "queued")
            {
                message.error = Some(format!("已保存，暂未投递：{error:#}"));
            }
            self.save(&state)?;
        }
        Ok(id)
    }
    pub fn read(&self, binding: &Binding) -> Result<Value> {
        self.validate(binding)?;
        let state = self.load()?;
        let result = self.client(&state)?.thread(&binding.thread)?;
        check_thread(binding, &result)?;
        Ok(result)
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let message = state
            .messages
            .iter_mut()
            .find(|message| message.id == id)
            .context("消息 ID 不存在")?;
        ensure!(
            matches!(message.status.as_str(), "queued" | "sending"),
            "只能取消排队或待核对记录；不会撤回已经发送的内容"
        );
        message.status = "cancelled".into();
        self.save(&state)
    }
    pub fn drain(&self) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let mut client = self.client(&state)?;
        // 显式恢复时补查漏掉的完成回调；所有绑定均由本功能从新 session 建立。
        for binding in state.bindings.values().cloned().collect::<Vec<_>>() {
            if binding.stopped || self.validate(&binding).is_err() {
                continue;
            }
            let result = client.thread(&binding.thread)?;
            check_thread(&binding, &result)?;
            if let Some(turns) = result["thread"]["turns"].as_array() {
                for turn in turns.iter().filter(|turn| turn["status"] == "completed") {
                    record_completion(
                        &mut state,
                        &binding,
                        turn["id"].as_str().context("轮次缺少 ID")?,
                    )?;
                }
            }
        }
        self.save(&state)?;
        drop(_lock);
        self.flush()
    }
    fn flush(&self) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let mut client = self.client(&state)?;
        for binding in state.bindings.values().cloned().collect::<Vec<_>>() {
            if binding.stopped {
                continue;
            }
            if !state.messages.iter().any(|m| {
                m.thread == binding.thread && matches!(m.status.as_str(), "queued" | "sending")
            }) {
                continue;
            }
            if let Err(error) = self.validate(&binding) {
                for message in state
                    .messages
                    .iter_mut()
                    .filter(|m| m.thread == binding.thread && m.status == "queued")
                {
                    message.status = "failed".into();
                    message.error = Some(format!("{error:#}"));
                }
                self.save(&state)?;
                continue;
            }
            let mut result = client.thread(&binding.thread)?;
            check_thread(&binding, &result)?;
            if result["thread"]["status"]["type"] == "notLoaded" {
                client.call("thread/resume", json!({"threadId":binding.thread,"excludeTurns":true,"config":{"notify":self.callback()?}}))?;
                result = client.thread(&binding.thread)?;
            }
            for message in state
                .messages
                .iter_mut()
                .filter(|m| m.thread == binding.thread && m.status == "sending")
            {
                if let Some(turn) = find_message(&result, &message.id) {
                    message.status = "sent".into();
                    message.turn = Some(turn);
                    message.error = None;
                }
            }
            self.save(&state)?;
            // 结果未知时不越过它发送后续消息。
            if state
                .messages
                .iter()
                .any(|m| m.thread == binding.thread && m.status == "sending")
                || result["thread"]["status"]["type"] != "idle"
            {
                continue;
            }
            if let Some(index) = state
                .messages
                .iter()
                .position(|m| m.thread == binding.thread && m.status == "queued")
            {
                let message = &mut state.messages[index];
                message.status = "sending".into();
                let params = json!({"threadId":binding.thread,"clientUserMessageId":message.id,"input":[{"type":"text","text":message.text,"text_elements":[]}]});
                self.save(&state)?;
                match client.call("turn/start", params) {
                    Ok(response) => {
                        state.messages[index].status = "sent".into();
                        state.messages[index].turn =
                            response["turn"]["id"].as_str().map(str::to_owned);
                    }
                    Err(error) => {
                        state.messages[index].error = Some(format!("投递结果未知：{error:#}"))
                    }
                }
                self.save(&state)?;
            }
        }
        Ok(())
    }
    pub fn event(&self, event: Value) -> Result<()> {
        ensure!(
            event["type"] == "agent-turn-complete",
            "不支持的 Codex 事件"
        );
        let thread = event["thread-id"].as_str().context("缺少 thread-id")?;
        let turn = event["turn-id"].as_str().context("缺少 turn-id")?;
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let binding = state
            .bindings
            .values()
            .find(|b| b.thread == thread)
            .cloned()
            .context("回调来自未绑定会话")?;
        if binding.stopped {
            return Ok(());
        }
        self.validate(&binding)?;
        let key = format!("{thread}:{turn}");
        if !state.seen.contains(&key) {
            // 以服务端状态验证轮次，不能将任意回调 JSON 当作完成事实。
            let result = self.client(&state)?.call(
                "thread/read",
                json!({"threadId":thread,"includeTurns":true}),
            )?;
            let completed = result["thread"]["turns"]
                .as_array()
                .and_then(|turns| turns.iter().find(|t| t["id"] == turn));
            check_thread(&binding, &result)?;
            ensure!(
                completed.is_some_and(|t| t["status"] == "completed"),
                "服务端尚未确认轮次完成，请用 twig agent drain 核对"
            );
            record_completion(&mut state, &binding, turn)?;
            self.save(&state)?;
        }
        drop(_lock);
        self.flush()
    }
    pub fn resume(&self, client: &mut rpc::Client, binding: &Binding) -> Result<bool> {
        self.validate(binding)?;
        let result = client.thread(&binding.thread)?;
        check_thread(binding, &result)?;
        // 新建且仍加载的空会话尚无 rollout，不能恢复；首条消息仍可直接投递。
        if result["thread"]["preview"]
            .as_str()
            .is_some_and(str::is_empty)
            && result["thread"]["status"]["type"] != "notLoaded"
        {
            return Ok(false);
        }
        client.call("thread/resume", json!({"threadId":binding.thread,"excludeTurns":true,"config":{"notify":self.callback()?}}))?;
        Ok(true)
    }
    pub fn stop_task(&self, thread: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let binding = state
            .bindings
            .get(thread)
            .context("任务会话绑定缺失")?
            .clone();
        self.validate(&binding)?;
        let mut client = self.client(&state)?;
        let result = client.thread(thread)?;
        check_thread(&binding, &result)?;
        // 先停止自动投递；中断失败仍保留停止标记和现场。
        state.bindings.get_mut(thread).unwrap().stopped = true;
        for message in state
            .messages
            .iter_mut()
            .filter(|m| m.thread == thread && m.status == "queued")
        {
            message.status = "cancelled".into();
        }
        self.save(&state)?;
        if let Some(turn) = result["thread"]["turns"]
            .as_array()
            .and_then(|turns| turns.iter().find(|t| t["status"] == "inProgress"))
        {
            client.call(
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn["id"]}),
            )?;
        }
        Ok(())
    }
    pub fn reactivate(&self, thread: &str) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.load()?;
        let binding = state.bindings.get(thread).context("任务会话绑定缺失")?;
        self.validate(binding)?;
        state.bindings.get_mut(thread).unwrap().stopped = false;
        self.save(&state)
    }
    pub fn removal_guard(&self, thread: &str) -> Result<impl Drop> {
        let lock = self.lock()?;
        let state = self.load()?;
        let result = self.client(&state)?.thread(thread)?;
        ensure!(
            matches!(
                result["thread"]["status"]["type"].as_str(),
                Some("idle" | "notLoaded")
            ),
            "受管 Codex 会话仍活动，不能删除；请先 twig stop"
        );
        ensure!(
            !state
                .messages
                .iter()
                .any(|m| m.thread == thread && matches!(m.status.as_str(), "queued" | "sending")),
            "任务仍有排队或结果未知的消息，请先核对或停止"
        );
        Ok(lock)
    }
}
fn record_completion(state: &mut State, binding: &Binding, turn: &str) -> Result<()> {
    let key = format!("{}:{turn}", binding.thread);
    if state.seen.contains(&key) {
        return Ok(());
    }
    if let Some(project) = &binding.project {
        let controller = state.bindings.get("controller").context("主控绑定缺失")?;
        state.messages.push(Delivery { id: message_id(), thread: controller.thread.clone(), text: format!("任务反馈：项目 {project}，任务 {}，会话 {}，轮次 {turn} 已完成。请读取任务 .twig/state.md 并用 twig agent read --project {project} --task {} 查询反馈，对照目标决定下一步；不要仅凭轮次完成宣布任务完成。", binding.task.as_deref().unwrap_or_default(), binding.thread, binding.task.as_deref().unwrap_or_default()), status: "queued".into(), turn: None, error: None });
    }
    state.seen.push(key);
    Ok(())
}

fn check_thread(binding: &Binding, result: &Value) -> Result<()> {
    ensure!(
        result["thread"]["id"] == binding.thread,
        "Codex 返回了不同会话"
    );
    let cwd = result["thread"]["cwd"]
        .as_str()
        .context("会话缺少工作目录")?;
    ensure!(
        Path::new(cwd).canonicalize()? == binding.cwd.canonicalize()?,
        "Codex 会话工作目录已变化；停止自动投递，请核对绑定"
    );
    Ok(())
}

fn find_message(value: &Value, id: &str) -> Option<String> {
    value["thread"]["turns"].as_array()?.iter().find(|turn| {
        turn["items"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "userMessage" && item["clientId"] == id)
        })
    })?["id"]
        .as_str()
        .map(str::to_owned)
}
fn message_id() -> String {
    format!(
        "twig-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
fn write_new(path: &Path, text: &str) -> Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => Ok(file.write_all(text.as_bytes())?),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn initialize_task_file(project: &crate::config::Project, cwd: &Path) -> Result<()> {
    let dir = cwd.join(".twig");
    if dir.symlink_metadata().is_ok() {
        ensure!(
            !dir.symlink_metadata()?.file_type().is_symlink() && dir.is_dir(),
            ".twig 必须是普通目录"
        );
    }
    fs::create_dir_all(&dir)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.join("state.md"))
        .context("任务 state.md 已存在或不可创建；保留现场，不覆盖")?;
    file.write_all("# 任务状态\n\n## 目标与验收标准\n待补充\n\n## 当前进展\n尚未开始\n\n## 阻塞与待决策\n无\n\n## 下一步\n等待派发\n\n## 验证结果\n尚未验证\n".as_bytes())?;
    let exclude = project.common.join("info/exclude");
    fs::create_dir_all(exclude.parent().unwrap())?;
    let previous = match fs::read_to_string(&exclude) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !previous.lines().any(|line| line == "/.twig/state.md") {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(exclude)?
            .write_all(b"\n/.twig/state.md\n")?;
    }
    Ok(())
}
pub fn task_layout(
    project: &crate::config::Project,
    thread: &str,
) -> Result<crate::startup::Layout> {
    let store = Store::new(&project.registry)?;
    store.reactivate(thread)?;
    let state = store.load()?;
    let socket = state.socket.context("受管任务缺少 Codex 服务绑定")?;
    let notify = toml::Value::try_from(store.callback()?)?.to_string();
    Ok(crate::startup::Layout {
        panes: vec![crate::startup::Pane {
            mode: crate::startup::Mode::Shell,
            command: Some(vec![
                "codex".into(),
                "resume".into(),
                "--remote".into(),
                format!("unix://{}", socket.display()),
                "-c".into(),
                format!("notify={notify}"),
                thread.into(),
            ]),
            split: None,
            percent: None,
            focus: true,
        }],
    })
}
pub fn execute(registry: &Path, action: Action) -> Result<()> {
    let store = Store::new(registry)?;
    match action {
        Action::Init => println!("{}", serde_json::to_string(&store.initialize()?)?),
        Action::Status => println!("{}", serde_json::to_string_pretty(&store.load()?)?),
        Action::Send { target, message } => {
            let binding = store.binding(&target)?;
            println!("{}", store.enqueue(&binding, message)?);
        }
        Action::Read { target } => println!(
            "{}",
            serde_json::to_string_pretty(&store.read(&store.binding(&target)?)?)?
        ),
        Action::Drain => store.drain()?,
        Action::Cancel { id } => store.cancel(&id)?,
        Action::Event { event } => store.event(serde_json::from_str(&event)?)?,
        Action::Notify { message } => notify(&message)?,
    }
    Ok(())
}
fn notify(message: &str) -> Result<()> {
    ensure!(!message.trim().is_empty(), "通知不能为空");
    let url = std::env::var("TWIG_FEISHU_WEBHOOK_URL")
        .context("请设置 TWIG_FEISHU_WEBHOOK_URL，并确保 Codex 执行通知 CLI 时可读取")?;
    ensure!(
        !url.contains(['\n', '\r'])
            && (url.starts_with("https://") || url.starts_with("http://127.0.0.1:")),
        "飞书 webhook 必须为 HTTPS URL"
    );
    let mut body = tempfile::NamedTempFile::new()?;
    serde_json::to_writer(
        &mut body,
        &json!({"msg_type":"text","content":{"text":message}}),
    )?;
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
        .arg(format!("@{}", body.path().display()))
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("飞书通知需要 curl")?;
    let url = url.replace('\\', "\\\\").replace('"', "\\\"");
    writeln!(child.stdin.take().unwrap(), "url = \"{url}\"")?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "飞书请求失败或超时（可能已送达，请核对后重试）"
    );
    let result: Value = serde_json::from_slice(&output.stdout).context("飞书响应不是 JSON")?;
    ensure!(
        result["code"] == 0,
        "飞书未接受通知，错误码：{}",
        result["code"]
    );
    println!("飞书已接受通知");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_delivery_matches_client_id_not_text() {
        let data = json!({"thread":{"turns":[{"id":"turn","items":[{"type":"userMessage","clientId":"one","content":[{"text":"same"}]}]}]}});
        assert_eq!(find_message(&data, "one").as_deref(), Some("turn"));
        assert_eq!(find_message(&data, "two"), None);
    }
    #[test]
    fn config_names_are_isolated_and_cancel_does_not_send() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::new(&root.path().join("config.toml")).unwrap();
        let second = Store::new(&root.path().join("other.toml")).unwrap();
        assert_ne!(first.dir, second.dir);
        let mut state = State::default();
        state.messages.push(Delivery {
            id: "unknown".into(),
            thread: "thread".into(),
            text: "work".into(),
            status: "sending".into(),
            turn: None,
            error: None,
        });
        {
            let _guard = first.lock().unwrap();
            first.save(&state).unwrap();
        }
        first.cancel("unknown").unwrap();
        assert_eq!(first.load().unwrap().messages[0].status, "cancelled");
        assert!(first.cancel("unknown").is_err());
        assert!(second.load().unwrap().messages.is_empty());
        assert!(!second.dir.exists());
    }
    #[test]
    fn task_state_never_overwrites_existing_file_or_symlink() {
        let root = tempfile::tempdir().unwrap();
        let project = crate::config::Project {
            common: root.path().join("git"),
            dir: root.path().join("project"),
            registry: root.path().join("config.toml"),
        };
        let worktree = root.path().join("worktree");
        fs::create_dir_all(&worktree).unwrap();
        initialize_task_file(&project, &worktree).unwrap();
        let file = worktree.join(".twig/state.md");
        fs::write(&file, "user state").unwrap();
        assert!(initialize_task_file(&project, &worktree).is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "user state");
        let other = root.path().join("other");
        fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(worktree.join(".twig"), other.join(".twig")).unwrap();
        assert!(initialize_task_file(&project, &other).is_err());
    }
}
