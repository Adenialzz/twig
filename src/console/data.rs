use super::query;
use crate::{
    codex::{Client, Thread},
    config::{Config, Feature, Project},
    git::git,
    registry, tmux,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Task {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub git: String,
    pub window: String,
    pub error: Option<String>,
    pub checked: Option<Instant>,
}
#[derive(Clone)]
pub struct Repo {
    pub name: String,
    pub path: PathBuf,
    pub tasks: Vec<Task>,
    pub error: Option<String>,
    pub context: Option<(Project, Config)>,
}
#[derive(Clone, Default)]
pub struct Snapshot {
    pub repos: Vec<Repo>,
    pub registry_error: Option<String>,
    pub registry_checked: Option<Instant>,
    pub threads: Vec<Thread>,
    pub codex_error: Option<String>,
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
#[derive(Clone, Default)]
pub struct Request {
    pub project: Option<String>,
    pub path: Option<PathBuf>,
    pub history: bool,
    pub descendants: bool,
    pub page: u64,
    pub turn: Option<String>,
    pub refresh: u64,
}

pub struct Worker {
    pub requests: Sender<Request>,
    pub updates: Receiver<Snapshot>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn start(registry: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let (out, updates) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let handle = thread::spawn(move || {
            query::install(stop.clone());
            collect(registry, rx, out, stop);
        });
        Self {
            requests: tx,
            updates,
            cancel,
            handle: Some(handle),
        }
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

pub fn owner<'a>(cwd: &Path, repos: &'a [Repo]) -> Option<(&'a str, &'a Path)> {
    let cwd = cwd.canonicalize().ok()?;
    repos
        .iter()
        .filter(|repo| repo.error.is_none())
        .flat_map(|repo| repo.tasks.iter().map(move |task| (repo, task)))
        .filter_map(|(repo, task)| {
            let path = task.path.canonicalize().ok()?;
            cwd.starts_with(&path).then_some((
                repo.name.as_str(),
                task.path.as_path(),
                path.components().count(),
            ))
        })
        .max_by_key(|(_, _, depth)| *depth)
        .map(|(repo, path, _)| (repo, path))
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

pub fn repositories(path: &Path) -> Result<Vec<Repo>> {
    registry::load(path)?
        .projects
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
                let project = Project::discover_at(&path)?;
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

fn refresh_git(repo: &mut Repo) {
    let Some((_, config)) = &repo.context else {
        return;
    };
    for task in &mut repo.tasks {
        if query::cancelled() {
            return;
        }
        let result = (|| -> Result<(String, String)> {
            ensure!(task.path.is_dir(), "工作目录缺失");
            if task.name != "main" {
                ensure!(
                    crate::check_worktree(
                        config,
                        &Feature {
                            path: task.path.clone(),
                            branch: task.branch.clone(),
                            base: String::new()
                        }
                    )?,
                    "Worktree 尚未创建"
                );
            }
            let branch = git(&task.path, &["branch", "--show-current"])?;
            let status = git(
                &task.path,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?;
            Ok((
                if branch.is_empty() {
                    "(detached)".into()
                } else {
                    branch
                },
                git_summary(&status),
            ))
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
fn refresh_windows(repo: &mut Repo) {
    let Some((project, config)) = &repo.context else {
        return;
    };
    let result = (|| -> Result<String> {
        if !tmux::session_exists(config, project)? {
            return Ok(String::new());
        }
        let output = tmux::tmux(&[
            "list-windows",
            "-t",
            &format!("={}", config.session),
            "-F",
            "#{window_id}\t#{window_name}\t#{@twig-feature}\t#{pane_start_path}",
        ])?;
        ensure!(
            output.lines().all(|line| line.split('\t').count() == 4),
            "无法解析 tmux 窗口信息"
        );
        Ok(output)
    })();
    for task in &mut repo.tasks {
        task.window = match &result {
            Err(error) => format!("查询失败：{error:#}"),
            Ok(text) => {
                let mut found = 0;
                let mut collision = false;
                for line in text.lines() {
                    let f: Vec<_> = line.split('\t').collect();
                    if f.len() != 4 {
                        continue;
                    }
                    if f[2] == task.name
                        || (task.name == "main"
                            && f[1] == "main"
                            && f[2].is_empty()
                            && Path::new(f[3]).canonicalize().ok().as_deref()
                                == Some(config.main.as_path()))
                    {
                        found += 1;
                    } else if f[1] == task.name {
                        collision = true;
                    }
                }
                if found > 1 {
                    "归属冲突：重复标记".into()
                } else if found == 1 {
                    "已打开".into()
                } else if collision {
                    "归属冲突：同名窗口".into()
                } else {
                    "未打开".into()
                }
            }
        };
    }
}

fn collect(
    registry: PathBuf,
    requests: Receiver<Request>,
    updates: Sender<Snapshot>,
    cancel: Arc<AtomicBool>,
) {
    let mut snapshot = Snapshot {
        codex_error: Some("尚未连接".into()),
        ..Default::default()
    };
    let mut request = Request::default();
    let mut client: Option<Client> = None;
    let mut registry_at = None;
    let mut activity_at = None;
    let mut git_at = None;
    let mut git_project = None;
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    while !cancel.load(Ordering::Relaxed) {
        let previous = request.clone();
        for next in requests.try_iter() {
            request = next;
        }
        let force = request.refresh != previous.refresh;
        let mut dirty = false;
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
            if updates.send(snapshot.clone()).is_err() {
                break;
            }
        }
        if force || due(activity_at, 2) {
            for repo in &mut snapshot.repos {
                if query::cancelled() {
                    return;
                }
                refresh_windows(repo);
            }
            let result = (|| -> Result<Vec<Thread>> {
                if client.is_none() {
                    client = Some(Client::discover()?);
                }
                client.as_mut().unwrap().loaded()
            })();
            match result {
                Ok(threads) => {
                    snapshot.threads = threads;
                    // 历史页曾经返回 active/idle；当前完整 loaded 快照已不含该 ID
                    // 时，其在本服务的状态应显示未加载，而非保留旧的运行状态。
                    for thread in &mut snapshot.history {
                        if !snapshot.threads.iter().any(|t| t.id == thread.id) {
                            thread.status = serde_json::json!({"type":"notLoaded"});
                        }
                    }
                    snapshot.codex_error = None;
                    snapshot.codex_checked = Some(Instant::now());
                }
                Err(e) => {
                    snapshot.codex_error = Some(format!("{e:#}"));
                    client = None;
                }
            }
            activity_at = Some(Instant::now());
            dirty = true;
        }
        // 合并采集期间的新选择，只查询当前需要的项目。
        for next in requests.try_iter() {
            request = next;
        }
        if force || request.project != git_project || due(git_at, 5) {
            if let Some(repo) = snapshot
                .repos
                .iter_mut()
                .find(|r| Some(&r.name) == request.project.as_ref())
            {
                refresh_git(repo);
            }
            git_at = Some(Instant::now());
            git_project = request.project.clone();
            dirty = true;
        }
        let key = request
            .path
            .clone()
            .filter(|_| request.history)
            .map(|path| (path, request.descendants));
        if key != snapshot.history_key || force {
            snapshot.history_key = key.clone();
            snapshot.history.clear();
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
                    snapshot.history_scanned += page.data.len();
                    snapshot.history_pages += 1;
                    for t in page.data {
                        if !t.ephemeral
                            && owner(&t.cwd, &snapshot.repos)
                                == request.project.as_deref().map(|p| (p, path.as_path()))
                            && !snapshot.history.iter().any(|old| old.id == t.id)
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
        if dirty {
            associate(&mut snapshot);
            if updates.send(snapshot.clone()).is_err() {
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}
fn associate(snapshot: &mut Snapshot) {
    snapshot.associations = snapshot
        .threads
        .iter()
        .chain(&snapshot.history)
        .filter_map(|t| {
            owner(&t.cwd, &snapshot.repos)
                .map(|(r, p)| (t.id.clone(), (r.to_owned(), p.to_owned())))
        })
        .collect();
}
fn due(at: Option<Instant>, seconds: u64) -> bool {
    at.is_none_or(|t| t.elapsed() >= Duration::from_secs(seconds))
}

pub fn enter(registry_path: &Path, name: &str, task: &str) -> Result<()> {
    // Enter 时重新解析登记信息，不能用陈旧快照决定写操作。
    let entry = registry::entry(registry_path, name)?;
    let path = registry::expand(&entry.path)?;
    let project = Project::discover_at(&path)?;
    let config = project.config()?;
    ensure!(
        path.canonicalize()? == config.main.canonicalize()?,
        "项目主目录归属已改变"
    );
    let lock = project.lock()?;
    let window = if task == "main" {
        tmux::ensure_main_window(&config, &project)?
    } else {
        let state = project.state()?;
        let feature = crate::feature(&state, task)?;
        ensure!(
            crate::check_worktree(&config, feature)?,
            "Worktree 缺失；请先检查 twig ls"
        );
        tmux::ensure_window(&config, &project, task, &feature.path)?
    };
    drop(lock);
    tmux::enter(&config, &window)
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "[projects.missing]\npath = {:?}\nmain = 'main'\n",
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
