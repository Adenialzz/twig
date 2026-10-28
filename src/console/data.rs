use super::{
    inbox::{Event as InboxEvent, Inbox},
    notifications::{ChatNotifications, Completion},
    query,
};
use crate::{
    codex::{Client, Thread},
    config::{Config, Feature, Project},
    git::git,
    registry,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub git: String,
    pub window: String,
    pub error: Option<String>,
    #[serde(with = "crate::daemon::time")]
    pub checked: Option<Instant>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub tasks: Vec<Task>,
    pub error: Option<String>,
    pub context: Option<(Project, Config)>,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub repos: Vec<Repo>,
    pub registry_error: Option<String>,
    #[serde(with = "crate::daemon::time")]
    pub registry_checked: Option<Instant>,
    pub threads: Vec<Thread>,
    pub codex_error: Option<String>,
    pub delivery_error: Option<String>,
    #[serde(with = "crate::daemon::time")]
    pub codex_checked: Option<Instant>,
    pub history: Vec<Thread>,
    pub history_key: Option<(PathBuf, bool)>,
    pub history_more: bool,
    pub history_pages: u64,
    pub history_scanned: usize,
    pub history_error: Option<String>,
    pub turn: Option<(String, Result<String, String>)>,
    pub associations: BTreeMap<String, (String, PathBuf)>,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub project: Option<String>,
    pub path: Option<PathBuf>,
    pub history: bool,
    pub descendants: bool,
    pub page: u64,
    pub turn: Option<String>,
    pub refresh: u64,
    pub background: bool,
    #[serde(default)]
    pub projects: Vec<String>,
}

// 快照可覆盖；完成事件使用独立队列，不能丢弃。
#[derive(Clone, Default)]
pub struct Updates(Arc<Mutex<Option<Snapshot>>>);
impl Updates {
    pub fn take(&self) -> Option<Snapshot> {
        self.0.lock().unwrap().take()
    }
    fn publish(&self, snapshot: &Snapshot) {
        let snapshot = snapshot.clone();
        *self.0.lock().unwrap() = Some(snapshot);
    }
}

pub struct Worker {
    pub requests: Sender<Request>,
    pub updates: Updates,
    pub inbox: Inbox,
    pub chat: Arc<Mutex<ChatNotifications>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(registry: PathBuf) -> Result<Self> {
        let inbox = Inbox::remote(&registry, crate::agent::message_id())?;
        let _ = inbox.sync();
        let saved = inbox.load().unwrap_or_default();
        let (tx, rx) = mpsc::channel::<Request>();
        let updates = Updates::default();
        let out = updates.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let chat = Arc::new(Mutex::new(ChatNotifications::persisted(
            inbox.clone(),
            &saved,
        )));
        let client = inbox.client_id().unwrap().to_owned();
        let remote_inbox = inbox.clone();
        let handle = thread::spawn(move || {
            let mut request = Request::default();
            let mut latest = Snapshot::default();
            let mut version = None::<u64>;
            let mut instance = serde_json::Value::Null;
            let mut detail_key = None;
            let mut detail = Snapshot::default();
            while !stop.load(Ordering::Relaxed) {
                for next in rx.try_iter() {
                    request = next;
                }
                let result = crate::daemon::call::<serde_json::Value>(
                    &registry,
                    "snapshot",
                    serde_json::json!({"client":client,"request":request,"version":version,"instance":instance}),
                );
                match result {
                    Ok(value) => {
                        if instance != value["instance"] {
                            detail_key = None;
                        }
                        instance = value["instance"].clone();
                        version = value["version"].as_u64();
                        if !value["snapshot"].is_null() {
                            match serde_json::from_value(value["snapshot"].clone()) {
                                Ok(snapshot) => latest = snapshot,
                                Err(error) => {
                                    latest.registry_error =
                                        Some(format!("后台快照格式错误：{error}"))
                                }
                            }
                        }
                    }
                    Err(error) => {
                        version = None;
                        latest.registry_error =
                            Some(format!("后台连接中断，旧数据过期：{error:#}"));
                    }
                }
                let key = serde_json::json!([
                    request.path,
                    request.history,
                    request.descendants,
                    request.page,
                    request.turn,
                    request.refresh
                ]);
                if !request.background && detail_key.as_ref() != Some(&key) {
                    detail_key = Some(key);
                    detail = Snapshot::default();
                    if request.history || request.turn.is_some() {
                        match crate::daemon::call::<Snapshot>(
                            &registry,
                            "details",
                            serde_json::json!(request),
                        ) {
                            Ok(snapshot) => detail = snapshot,
                            Err(error) => detail.history_error = Some(format!("{error:#}")),
                        }
                    }
                }
                latest.history = detail.history.clone();
                latest.history_key = detail.history_key.clone();
                latest.history_more = detail.history_more;
                latest.history_pages = detail.history_pages;
                latest.history_scanned = detail.history_scanned;
                latest.history_error = detail.history_error.clone();
                latest.turn = detail.turn.clone();
                latest.associations.extend(detail.associations.clone());
                if let Err(error) = remote_inbox.sync() {
                    latest.delivery_error = Some(format!("未读同步失败：{error:#}"));
                }
                out.publish(&latest);
                for _ in 0..5 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
            let _ = remote_inbox.sync();
            let _ = crate::daemon::call::<serde_json::Value>(
                &registry,
                "disconnect",
                serde_json::json!(client),
            );
        });
        Ok(Self {
            requests: tx,
            updates,
            inbox,
            chat,
            cancel,
            handle: Some(handle),
        })
    }
    pub fn start_local(registry: PathBuf) -> Result<Self> {
        let inbox = Inbox::new(&registry)?;
        let saved = inbox.load()?;
        let (tx, rx) = mpsc::channel();
        let updates = Updates::default();
        let out = updates.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let chat = Arc::new(Mutex::new(ChatNotifications::persisted(
            inbox.clone(),
            &saved,
        )));
        let inbox_out = inbox.clone();
        let chat_out = chat.clone();
        let handle = thread::spawn(move || {
            query::install(stop.clone());
            collect(registry, rx, out, inbox_out, saved, stop, chat_out);
        });
        Ok(Self {
            requests: tx,
            updates,
            inbox,
            chat,
            cancel,
            handle: Some(handle),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Owners {
    paths: Vec<(PathBuf, String, PathBuf)>,
    resolved: BTreeMap<PathBuf, Option<(String, PathBuf)>>,
}
impl Owners {
    fn new(repos: &[Repo]) -> Self {
        let mut paths: Vec<_> = repos
            .iter()
            .filter(|r| r.error.is_none())
            .flat_map(|r| r.tasks.iter().map(move |t| (r, t)))
            .filter_map(|(r, t)| {
                Some((t.path.canonicalize().ok()?, r.name.clone(), t.path.clone()))
            })
            .collect();
        paths.sort_by_key(|(path, _, _)| std::cmp::Reverse(path.components().count()));
        Self {
            paths,
            resolved: BTreeMap::new(),
        }
    }
    fn get(&mut self, cwd: &Path) -> Option<(String, PathBuf)> {
        self.resolved
            .entry(cwd.to_owned())
            .or_insert_with(|| {
                let cwd = cwd.canonicalize().ok()?;
                self.paths
                    .iter()
                    .find(|(path, _, _)| cwd.starts_with(path))
                    .map(|(_, r, p)| (r.clone(), p.clone()))
            })
            .clone()
    }
}
#[cfg(test)]
pub fn owner<'a>(cwd: &Path, repos: &'a [Repo]) -> Option<(&'a str, &'a Path)> {
    let (name, path) = Owners::new(repos).get(cwd)?;
    let repo = repos.iter().find(|r| r.name == name)?;
    Some((
        &repo.name,
        &repo.tasks.iter().find(|t| t.path == path)?.path,
    ))
}
pub fn for_task<'a>(
    threads: &'a [Thread],
    snapshot: &Snapshot,
    project: &str,
    path: &Path,
) -> Vec<&'a Thread> {
    let mut result: Vec<_> = threads
        .iter()
        .filter(|thread| {
            snapshot
                .associations
                .get(&thread.id)
                .is_some_and(|(r, p)| r == project && p == path)
        })
        .collect();
    result.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id.cmp(&b.id)));
    result
}
pub fn summary<'a>(threads: impl Iterator<Item = &'a Thread>) -> String {
    let mut counts = BTreeMap::new();
    for thread in threads {
        *counts.entry(thread.label()).or_insert(0) += 1;
    }
    if counts.is_empty() {
        "—".into()
    } else {
        counts
            .into_iter()
            .map(|(label, n)| format!("{label} {n}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn repositories(registry_path: &Path) -> Result<Vec<Repo>> {
    registry::recent_projects(registry_path)?
        .into_iter()
        .map(|(name, entry)| {
            query::check()?;
            let path = registry::expand(&entry.path)?;
            let mut repo = Repo {
                name,
                path: path.clone(),
                tasks: vec![],
                error: None,
                context: None,
            };
            let result = (|| -> Result<()> {
                let canonical = path.canonicalize().context("登记路径不存在或无法访问")?;
                ensure!(
                    PathBuf::from(git(&path, &["rev-parse", "--show-toplevel"])?).canonicalize()?
                        == canonical,
                    "登记路径不是仓库根目录"
                );
                let project = Project::discover_at(&path, registry_path)?;
                let config = project.config()?;
                ensure!(
                    config.main.canonicalize()? == canonical,
                    "登记路径与项目主目录不一致"
                );
                repo.tasks.push(Task {
                    name: "main".into(),
                    path: config.main.clone(),
                    branch: String::new(),
                    git: "待查询".into(),
                    window: "待查询".into(),
                    error: None,
                    checked: None,
                });
                for (name, f) in project.state()? {
                    repo.tasks.push(Task {
                        name,
                        path: f.path,
                        branch: f.branch,
                        git: "待查询".into(),
                        window: "待查询".into(),
                        error: None,
                        checked: None,
                    });
                }
                repo.tasks.sort_by_key(|task| {
                    std::cmp::Reverse(entry.task_last_used.get(&task.name).copied())
                });
                repo.context = Some((project, config));
                Ok(())
            })();
            if let Err(error) = result {
                repo.error = Some(format!("{error:#}"));
            }
            Ok(repo)
        })
        .collect()
}

struct GitScan {
    project: String,
    common: PathBuf,
    trees: Result<Vec<(PathBuf, String)>>,
    paths: std::collections::VecDeque<PathBuf>,
}
impl GitScan {
    fn new(repo: &Repo, selected: Option<&Path>) -> Option<Self> {
        let (project, config) = repo.context.as_ref()?;
        let mut paths: Vec<_> = repo.tasks.iter().map(|t| t.path.clone()).collect();
        paths.sort_by_key(|path| Some(path.as_path()) != selected);
        Some(Self {
            project: repo.name.clone(),
            common: project.common.clone(),
            trees: crate::git::worktrees(&config.main),
            paths: paths.into(),
        })
    }
    fn next(&mut self, repo: &mut Repo) {
        let Some(path) = self.paths.pop_front() else {
            return;
        };
        let Some(task) = repo.tasks.iter_mut().find(|t| t.path == path) else {
            return;
        };
        let result = (|| -> Result<(String, String)> {
            ensure!(task.path.is_dir(), "工作目录缺失");
            if task.name != "main" {
                let trees = self.trees.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?;
                ensure!(
                    crate::check_worktree_in(
                        trees,
                        &self.common,
                        &Feature {
                            path: task.path.clone(),
                            branch: task.branch.clone(),
                            base: String::new(),
                            remote_start: None,
                            agent_thread: None,
                        }
                    )?,
                    "Worktree 尚未创建"
                );
            }
            let status = git(
                &task.path,
                &[
                    "status",
                    "--porcelain=v1",
                    "--branch",
                    "-z",
                    "--untracked-files=all",
                ],
            )?;
            branch_status(&status)
        })();
        match result {
            Ok((branch, status)) => {
                task.branch = branch;
                task.git = status;
                task.error = None;
                task.checked = Some(Instant::now());
            }
            Err(error) => {
                task.error = Some(format!("{error:#}"));
                task.git = "异常 / 旧值过期".into();
            }
        }
    }
}
fn branch_status(status: &str) -> Result<(String, String)> {
    let (header, files) = status.split_once('\0').context("Git 状态缺少分支头")?;
    let branch = header.strip_prefix("## ").context("Git 分支头格式异常")?;
    let branch = if branch == "HEAD (no branch)" {
        "(detached)"
    } else {
        branch
            .strip_prefix("No commits yet on ")
            .or_else(|| branch.strip_prefix("Initial commit on "))
            .unwrap_or(branch)
            .split("...")
            .next()
            .unwrap()
    };
    Ok((branch.to_owned(), git_summary(files)))
}
fn git_summary(status: &str) -> String {
    let (mut staged, mut modified, mut untracked) = (0, 0, 0);
    let mut fields = status.split('\0');
    while let Some(field) = fields.next() {
        let b = field.as_bytes();
        if b.len() < 3 {
            continue;
        }
        if &b[..2] == b"??" {
            untracked += 1;
        } else {
            if b[0] != b' ' {
                staged += 1;
            }
            if b[1] != b' ' {
                modified += 1;
            }
            if matches!(b[0], b'R' | b'C') || matches!(b[1], b'R' | b'C') {
                fields.next();
            }
        }
    }
    if staged + modified + untracked == 0 {
        "干净".into()
    } else {
        format!("暂存 {staged} / 修改 {modified} / 新增 {untracked}")
    }
}
fn refresh_windows(repos: &mut [Repo]) {
    if repos.iter().all(|repo| repo.context.is_none()) {
        return;
    }
    let result = (|| -> Result<BTreeMap<String, Vec<Vec<String>>>> {
        let output = crate::git::run("tmux", &[
            "list-windows".as_ref(), "-a".as_ref(), "-F".as_ref(),
            "#{session_name}\t#{@twig-repo}\t#{window_id}\t#{window_name}\t#{@twig-feature}\t#{pane_start_path}".as_ref(),
        ], None)?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("no server running") || error.contains("No such file or directory") {
                return Ok(BTreeMap::new());
            }
            anyhow::bail!("无法查询 tmux：{}", error.trim());
        }
        let text = String::from_utf8(output.stdout)?;
        let mut sessions: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for line in text.lines() {
            let fields: Vec<_> = line.split('\t').map(str::to_owned).collect();
            ensure!(fields.len() == 6, "无法解析 tmux 窗口信息");
            sessions.entry(fields[0].clone()).or_default().push(fields);
        }
        Ok(sessions)
    })();
    for repo in repos {
        let Some((project, config)) = &repo.context else {
            continue;
        };
        let windows = result
            .as_ref()
            .map_err(|e| format!("{e:#}"))
            .and_then(|sessions| {
                let rows = sessions
                    .get(&config.session)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                if rows
                    .iter()
                    .any(|f| f[1] != project.common.to_string_lossy())
                {
                    Err(format!(
                        "同名 tmux Session 不属于本项目：{}",
                        config.session
                    ))
                } else {
                    Ok(rows)
                }
            });
        let mut marked = BTreeMap::<&str, usize>::new();
        let mut names = HashSet::new();
        if let Ok(rows) = &windows {
            for f in *rows {
                names.insert(f[3].as_str());
                let name = if f[4].is_empty()
                    && f[3] == "main"
                    && Path::new(&f[5]).canonicalize().ok().as_deref()
                        == Some(config.main.as_path())
                {
                    "main"
                } else {
                    f[4].as_str()
                };
                *marked.entry(name).or_default() += 1;
            }
        }
        for task in &mut repo.tasks {
            task.window = match &windows {
                Err(error) => format!("查询失败：{error}"),
                Ok(_) => match marked.get(task.name.as_str()).copied().unwrap_or(0) {
                    0 if names.contains(task.name.as_str()) => "归属冲突：同名窗口".into(),
                    0 => "未打开".into(),
                    1 => "已打开".into(),
                    _ => "归属冲突：重复标记".into(),
                },
            };
        }
    }
}

// 只有成功读取后确认取消登记或实例变化才终止处理；读失败不能当作已失效。
fn message_task_current(
    store: &crate::agent::Store,
    binding: &crate::agent::Binding,
    alias: &str,
    name: &str,
) -> Result<bool> {
    std::fs::metadata(&store.registry).context("无法读取项目登记表")?;
    let registry = registry::load(&store.registry)?;
    let Some(entry) = registry.projects.get(alias) else {
        return Ok(false);
    };
    let project = Project::discover_at(&registry::expand(&entry.path)?, &store.registry)?;
    let state: crate::config::State = serde_json::from_slice(
        &std::fs::read(project.dir.join("state.json")).context("无法读取任务记录")?,
    )?;
    let Some(task) = state.get(name) else {
        return Ok(false);
    };
    if task.agent_thread.as_deref() != Some(&binding.thread) || task.path != binding.cwd {
        return Ok(false);
    }
    ensure!(
        crate::check_worktree(&project.config()?, task)?,
        "任务 Worktree 不存在"
    );
    Ok(true)
}

fn collect(
    registry: PathBuf,
    requests: Receiver<Request>,
    updates: Updates,
    inbox: Inbox,
    saved: super::inbox::State,
    cancel: Arc<AtomicBool>,
    chat: Arc<Mutex<ChatNotifications>>,
) {
    let mut snapshot = Snapshot {
        codex_error: Some("尚未连接".into()),
        ..Default::default()
    };
    let mut request = Request::default();
    let mut client: Option<Client> = None;
    let mut watched = saved.watched;
    let mut catchup: HashSet<_> = watched.keys().cloned().collect();
    let mut deliveries = saved.deliveries;
    let mut registry_at = None;
    let mut activity_at = None;
    let mut retry_at = None;
    let mut retry_delay = 2;
    let mut git_at = None;
    let mut git_project = None;
    let mut git_scan: Option<GitScan> = None;
    let mut next_project = 0;
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut handled_refresh = 0;
    let mut revision = crate::daemon::REVISION.load(Ordering::Relaxed);
    let mut unpublished = false;
    let mut published_at = Instant::now();
    while !cancel.load(Ordering::Relaxed) {
        for next in requests.try_iter() {
            request = next;
        }
        let next_revision = crate::daemon::REVISION.load(Ordering::Relaxed);
        let force = request.refresh != handled_refresh || revision != next_revision;
        revision = next_revision;
        handled_refresh = request.refresh;
        let mut dirty = false;
        let mut associations_changed = false;
        if force || due(registry_at, 10) {
            match repositories(&registry) {
                Ok(mut repos) => {
                    for repo in &mut repos {
                        if let Some(old) = snapshot
                            .repos
                            .iter()
                            .find(|r| r.name == repo.name && r.path == repo.path)
                        {
                            for task in &mut repo.tasks {
                                if let Some(t) = old.tasks.iter().find(|t| {
                                    t.name == task.name
                                        && t.path == task.path
                                        && (task.name == "main" || t.branch == task.branch)
                                }) {
                                    task.git = t.git.clone();
                                    task.window = t.window.clone();
                                    task.error = t.error.clone();
                                    task.checked = t.checked;
                                    if task.name == "main" {
                                        task.branch = t.branch.clone();
                                    }
                                }
                            }
                        }
                    }
                    snapshot.repos = repos;
                    snapshot.registry_error = None;
                    snapshot.registry_checked = Some(Instant::now());
                }
                Err(e) => {
                    snapshot.registry_error = Some(format!("登记表读取失败，旧数据过期：{e:#}"))
                }
            }
            registry_at = Some(Instant::now());
            dirty = true;
            // 先发布项目列表，Codex 或 Git 慢时仍可开始导航。
            associate(&mut snapshot);
            updates.publish(&snapshot);
        }
        if force || due(activity_at, 2) {
            if query::cancelled() {
                return;
            }
            refresh_windows(&mut snapshot.repos);
            updates.publish(&snapshot);
            // 本地投递记录不依赖 daemon 连接或重试退避；采集本身不投递消息。
            let state = crate::agent::Store::new(&registry).and_then(|store| {
                let state = store.load()?;
                inbox.controller(
                    state
                        .bindings
                        .get("controller")
                        .map(|binding| binding.thread.as_str()),
                )?;
                let mut errors = Vec::new();
                let mut obsolete = HashSet::new();
                for event in inbox.load()?.unread {
                    let Some(task) = &event.task else { continue };
                    let binding = state
                        .bindings
                        .values()
                        .find(|binding| binding.thread == event.thread);
                    if let Some(binding) = binding.filter(|binding| binding.project.is_some()) {
                        match message_task_current(&store, binding, &task.project, &task.task) {
                            Ok(false) => {
                                obsolete.insert(event.id);
                            }
                            Err(error) => errors.push(format!("未读归属检查失败：{error:#}")),
                            Ok(true) => {}
                        }
                    } else if task.message {
                        obsolete.insert(event.id);
                    }
                }
                inbox.discard(&obsolete)?;
                errors.extend(deliveries.observe(&state, |binding, id| {
                    if let (Some(project), Some(task)) = (&binding.project, &binding.task) {
                        if message_task_current(&store, binding, project, task)? {
                            inbox.record(
                                InboxEvent {
                                    id: format!("message:{id}"),
                                    thread: binding.thread.clone(),
                                    task: Some(Completion {
                                        message: true,
                                        project: project.clone(),
                                        task: task.clone(),
                                        path: binding.cwd.clone(),
                                    }),
                                },
                                false,
                            )?;
                        }
                    } else if state
                        .bindings
                        .get("controller")
                        .is_some_and(|b| b.thread == binding.thread)
                    {
                        chat.lock().unwrap().record(InboxEvent {
                            id: format!("message:{id}"),
                            thread: binding.thread.clone(),
                            task: None,
                        })?;
                    }
                    Ok(())
                }));
                snapshot.delivery_error = (!errors.is_empty()).then(|| errors.join("\n"));
                Ok(state)
            });
            if let Err(error) = &state {
                snapshot.delivery_error = Some(format!("消息记录读取失败：{error:#}"));
            }
            if force || retry_at.is_none_or(|at: Instant| Instant::now() >= at) {
                let result = (|| -> Result<Vec<Thread>> {
                    let state = state?;
                    if client.is_none() {
                        client = Some(Client::discover()?);
                    }
                    let client = client.as_mut().unwrap();
                    let threads = client.loaded()?;
                    let controller = state.bindings.get("controller");
                    let mut owners = Owners::new(&snapshot.repos);
                    let mut paths: BTreeMap<_, _> = threads
                        .iter()
                        .filter(|t| {
                            owners.get(&t.cwd).is_some()
                                || controller.is_some_and(|c| c.thread == t.id)
                        })
                        .map(|t| (t.id.clone(), t.cwd.clone()))
                        .collect();
                    // 重启期间会话可能已完成并卸载；继续补查所有已观察会话。
                    for (id, (path, turn)) in &watched {
                        if catchup.contains(id)
                            || turn
                                .as_ref()
                                .is_some_and(|turn| turn.status == "inProgress")
                        {
                            paths.entry(id.clone()).or_insert_with(|| path.clone());
                        }
                    }
                    let mut errors = Vec::new();
                    for (id, path) in paths {
                        let result = (|| -> Result<()> {
                            if controller.is_some_and(|c| c.thread == id) {
                                let (latest, turns) = client
                                    .completed_turns(&id, watched.get(&id).map(|(_, turn)| turn))?;
                                for turn in turns {
                                    chat.lock().unwrap().record(InboxEvent {
                                        id: format!("turn:{id}:{}", turn.id),
                                        thread: id.clone(),
                                        task: None,
                                    })?;
                                }
                                watched.insert(id.clone(), (path, latest));
                                catchup.remove(&id);
                                return Ok(());
                            }
                            if let Some(binding) =
                                state.bindings.values().find(|binding| binding.thread == id)
                                && let (Some(project), Some(task)) =
                                    (&binding.project, &binding.task)
                                && !message_task_current(
                                    &crate::agent::Store::new(&registry)?,
                                    binding,
                                    project,
                                    task,
                                )?
                            {
                                return Ok(());
                            }
                            let Some((project, task_path)) = owners.get(&path) else {
                                return Ok(());
                            };
                            let (latest, turns) = client
                                .completed_turns(&id, watched.get(&id).map(|(_, turn)| turn))?;
                            for turn in turns {
                                let task = snapshot
                                    .repos
                                    .iter()
                                    .find(|r| r.name == project)
                                    .unwrap()
                                    .tasks
                                    .iter()
                                    .find(|t| t.path == task_path)
                                    .unwrap();
                                inbox.record(
                                    InboxEvent {
                                        id: format!("turn:{id}:{}", turn.id),
                                        thread: id.clone(),
                                        task: Some(Completion {
                                            message: false,
                                            project: project.to_owned(),
                                            path: task_path.to_owned(),
                                            task: task.name.clone(),
                                        }),
                                    },
                                    false,
                                )?;
                            }
                            watched.insert(id.clone(), (path, latest));
                            catchup.remove(&id);
                            Ok(())
                        })();
                        if let Err(error) = result {
                            // 卸载不代表完成或空历史；保留水位，待重新加载后补查。
                            if crate::codex::thread_not_loaded(&error, "thread/turns/list", &id) {
                                continue;
                            }
                            errors.push(format!("会话 {id}：{error:#}"));
                        }
                    }
                    ensure!(errors.is_empty(), "{}", errors.join("\n"));
                    Ok(threads)
                })();
                match result {
                    Ok(threads) => {
                        snapshot.threads = threads;
                        associations_changed = true;
                        // 历史页曾经返回 active/idle；当前完整 loaded 快照已不含该 ID
                        // 时，其在本服务的状态应显示未加载，而非保留旧的运行状态。
                        let loaded: HashSet<_> =
                            snapshot.threads.iter().map(|t| t.id.as_str()).collect();
                        for thread in &mut snapshot.history {
                            if !loaded.contains(thread.id.as_str()) {
                                thread.status = serde_json::json!({"type":"notLoaded"});
                            }
                        }
                        snapshot.codex_error = None;
                        snapshot.codex_checked = Some(Instant::now());
                        retry_at = None;
                        retry_delay = 2;
                    }
                    Err(e) => {
                        snapshot.codex_error = Some(format!("{e:#}"));
                        client = None;
                        retry_at = Some(Instant::now() + Duration::from_secs(retry_delay));
                        retry_delay = (retry_delay * 2).min(30);
                    }
                }
            }
            if let Err(error) = inbox.checkpoint(&deliveries, &watched) {
                snapshot.delivery_error = Some(format!("未读进度保存失败：{error:#}"));
            }
            activity_at = Some(Instant::now());
            dirty = true;
        }
        // 合并采集期间的新选择，只查询当前需要的项目。
        for next in requests.try_iter() {
            request = next;
        }
        if force || request.project != git_project || request.background {
            git_scan = None;
            git_at = None;
            git_project = request.project.clone();
        }
        if !request.background {
            if git_scan.is_none() && due(git_at, 5) {
                git_scan = snapshot
                    .repos
                    .iter()
                    .filter(|r| {
                        request.projects.contains(&r.name)
                            || Some(&r.name) == request.project.as_ref()
                    })
                    .nth(next_project)
                    .and_then(|repo| GitScan::new(repo, request.path.as_deref()));
                if git_scan.is_none() {
                    next_project = 0;
                    git_at = Some(Instant::now());
                }
            }
            if let Some(scan) = &mut git_scan {
                if let Some(repo) = snapshot.repos.iter_mut().find(|r| r.name == scan.project) {
                    scan.next(repo);
                    dirty = true;
                } else {
                    scan.paths.clear();
                }
                if scan.paths.is_empty() {
                    git_scan = None;
                    next_project += 1;
                    git_at = None;
                }
            }
        }
        let key = request
            .path
            .clone()
            .filter(|_| request.history)
            .map(|path| (path, request.descendants));
        if key != snapshot.history_key || force {
            snapshot.history_key = key.clone();
            snapshot.history.clear();
            associations_changed = true;
            snapshot.history_pages = 0;
            snapshot.history_scanned = 0;
            snapshot.history_more = true;
            snapshot.history_error = None;
            cursor = None;
            seen_cursors.clear();
            dirty = true;
        }
        if let Some((path, descendants)) = key
            && snapshot.history_more
            && snapshot.history_pages <= request.page
            && snapshot.history_error.is_none()
        {
            let result = (|| -> Result<crate::codex::Page> {
                let client = client.as_mut().context("Codex 未连接；r 重试")?;
                client.history(
                    if descendants {
                        None
                    } else {
                        Some(path.as_path())
                    },
                    cursor.as_deref(),
                )
            })();
            match result {
                Ok(page) => {
                    associations_changed = true;
                    snapshot.history_scanned += page.data.len();
                    snapshot.history_pages += 1;
                    let mut owners = Owners::new(&snapshot.repos);
                    let mut ids: HashSet<_> =
                        snapshot.history.iter().map(|t| t.id.clone()).collect();
                    for t in page.data {
                        if !t.ephemeral
                            && owners.get(&t.cwd)
                                == request.project.clone().map(|p| (p, path.clone()))
                            && ids.insert(t.id.clone())
                        {
                            snapshot.history.push(t);
                        }
                    }
                    cursor = page.next_cursor;
                    snapshot.history_more = cursor.is_some();
                    if let Some(c) = &cursor
                        && !seen_cursors.insert(c.clone())
                    {
                        snapshot.history_error = Some("Codex 返回重复分页游标".into());
                        snapshot.history_more = false;
                    }
                }
                Err(e) => {
                    snapshot.history_error = Some(format!("{e:#}"));
                    client = None;
                }
            }
            dirty = true;
        }
        if let Some(id) = &request.turn
            && (force || snapshot.turn.as_ref().is_none_or(|(old, _)| old != id))
        {
            let value = client
                .as_mut()
                .context("Codex 未连接")
                .and_then(|c| c.latest_turn(id))
                .map_err(|e| format!("{e:#}"));
            if value.is_err() {
                client = None;
            }
            snapshot.turn = Some((id.clone(), value));
            dirty = true;
        }
        if associations_changed {
            associate(&mut snapshot);
        }
        unpublished |= dirty;
        if unpublished
            && (git_scan.is_none() || published_at.elapsed() >= Duration::from_millis(100))
        {
            updates.publish(&snapshot);
            unpublished = false;
            published_at = Instant::now();
        }
        thread::sleep(Duration::from_millis(if git_scan.is_some() {
            1
        } else {
            50
        }));
    }
}
pub(crate) fn associate(snapshot: &mut Snapshot) {
    let mut owners = Owners::new(&snapshot.repos);
    snapshot.associations = snapshot
        .threads
        .iter()
        .chain(&snapshot.history)
        .filter_map(|t| owners.get(&t.cwd).map(|owner| (t.id.clone(), owner)))
        .collect();
}
fn due(at: Option<Instant>, seconds: u64) -> bool {
    at.is_none_or(|t| t.elapsed() >= Duration::from_secs(seconds))
}

pub fn prepare_terminal(registry_path: &Path, name: &str, task: &str) -> Result<(String, String)> {
    let output = crate::daemon::operations::execute(
        registry_path,
        crate::Action::Open {
            project: name.into(),
            name: task.into(),
        },
    )?;
    let (_, target) = output.terminal.context("后台未返回终端目标")?;
    Ok((target.session, target.window))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_header_keeps_upstream_detached_and_rename_counts() {
        assert_eq!(
            branch_status("## topic...origin/topic [ahead 1]\0R  new\0old\0?? extra\0").unwrap(),
            ("topic".into(), "暂存 1 / 修改 0 / 新增 1".into())
        );
        assert_eq!(
            branch_status("## HEAD (no branch)\0").unwrap(),
            ("(detached)".into(), "干净".into())
        );
        assert!(branch_status("invalid").is_err());
    }

    #[test]
    fn pending_snapshots_replace_old_values_without_dropping_completion_events() {
        let updates = Updates::default();
        let (out, events) = mpsc::channel();
        for index in 0..3 {
            updates.publish(&Snapshot {
                history_pages: index,
                ..Default::default()
            });
            out.send(Completion {
                message: false,
                project: "repo".into(),
                path: "/repo".into(),
                task: index.to_string(),
            })
            .unwrap();
        }
        assert_eq!(updates.take().unwrap().history_pages, 2);
        assert!(updates.take().is_none());
        assert_eq!(
            events
                .try_iter()
                .map(|event| event.task)
                .collect::<Vec<_>>(),
            ["0", "1", "2"]
        );
    }
    fn repo(name: &str, path: PathBuf) -> Repo {
        Repo {
            name: name.into(),
            path: path.clone(),
            tasks: vec![Task {
                name: "main".into(),
                path,
                branch: "main".into(),
                git: String::new(),
                window: String::new(),
                error: None,
                checked: None,
            }],
            error: None,
            context: None,
        }
    }
    #[test]
    fn cwd_matching_uses_deepest_components_and_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let a = root.join("a");
        let nested = a.join("nested");
        let sibling = root.join("ab");
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let repos = vec![repo("a", a.clone()), repo("nested", nested.clone())];
        assert_eq!(
            owner(&nested.join("src"), &repos),
            Some(("nested", nested.as_path()))
        );
        assert!(owner(&sibling, &repos).is_none());
        assert!(owner(&a.join("missing"), &repos).is_none());
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        assert_eq!(owner(&alias, &repos), Some(("nested", nested.as_path())));
    }
    #[test]
    fn porcelain_rename_does_not_count_original_path_as_a_file() {
        assert_eq!(
            git_summary("R  dest\0?? original\0 M modified\0?? new\0"),
            "暂存 1 / 修改 1 / 新增 1"
        );
        assert_eq!(git_summary(""), "干净");
    }
    #[test]
    fn broken_project_does_not_hide_others_or_create_registry() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry.toml");
        assert!(repositories(&registry).unwrap().is_empty());
        assert!(!registry.exists());
        std::fs::write(
            &registry,
            format!(
                "[projects.missing]\npath = {:?}\n",
                dir.path().join("missing")
            ),
        )
        .unwrap();
        let repos = repositories(&registry).unwrap();
        assert_eq!(repos.len(), 1);
        assert!(repos[0].error.is_some());
        assert!(!dir.path().join("missing").exists());
    }
}
