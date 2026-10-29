//! 持久化业务操作：客户端断开不取消执行，未知结果不自动重试。
mod store;
use super::output::Output;
use crate::{Action, config::Project, registry};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use store::Store;

#[derive(Args, Clone, Serialize, Deserialize)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Subcommand, Clone, Serialize, Deserialize)]
pub enum Command {
    /// 列出已接受的操作（JSON）
    Ls,
    /// 按操作 ID 或请求 ID 查询（JSON）
    Show { id: String },
    /// 核对资源后解除待核对标记；不会重新执行、回滚或宣称成功
    Resolve {
        id: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub expires: u64,
    pub action: Action,
    pub fingerprint: Value,
    pub branches: Vec<crate::config::BranchStart>,
    pub common: Option<PathBuf>,
    pub prompt: Option<String>,
}
type Checkpoint = Box<dyn Fn(&str) -> Result<()>>;
thread_local! { static CHECKPOINT: RefCell<Option<Checkpoint>> = const { RefCell::new(None) }; }
pub fn progress(stage: &str) -> Result<()> {
    CHECKPOINT.with(|callback| match callback.borrow().as_ref() {
        Some(callback) => callback(stage),
        None => Ok(()),
    })
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub request: String,
    pub plan: Plan,
    pub status: String,
    #[serde(default)]
    pub stages: Vec<String>,
    pub output: Output,
    pub error: Option<String>,
}
#[derive(Clone)]
pub struct Service {
    registry: PathBuf,
    store: Store,
    records: Arc<Mutex<BTreeMap<String, Operation>>>,
    plans: Arc<Mutex<BTreeMap<String, Plan>>>,
    queue: mpsc::SyncSender<String>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
// 已接受的操作不能因存储故障一直显示为运行中，或把未持久化的终态当作成功。
fn save_accepted(store: &Store, records: &mut BTreeMap<String, Operation>, id: &str) -> Result<()> {
    if let Err(error) = store.save(records.get(id).context("操作记录不存在")?) {
        let operation = records.get_mut(id).context("操作记录不存在")?;
        operation.status = "needs_inspection".into();
        operation.error = Some(format!("操作记录保存失败，请恢复存储后核对资源：{error:#}"));
        return Err(error);
    }
    Ok(())
}
impl Service {
    pub fn start(registry: &Path, directory: &Path) -> Result<(Self, thread::JoinHandle<()>)> {
        let (store, mut records) = Store::open(directory)?;
        for record in records
            .values_mut()
            .filter(|r| matches!(r.status.as_str(), "running" | "queued"))
        {
            record.status = "needs_inspection".into();
            record.error =
                Some("后台在操作完成前退出；请核对资源和确认有效性，不自动重新执行".into());
            store.save(record)?;
        }
        let (queue, receiver) = mpsc::sync_channel::<String>(64);
        let service = Self {
            registry: registry.to_owned(),
            store,
            records: Arc::new(Mutex::new(records)),
            plans: Arc::new(Mutex::new(BTreeMap::new())),
            queue,
        };
        let registry = service.registry.clone();
        let store = service.store.clone();
        let records = service.records.clone();
        let worker = thread::spawn(move || {
            while let Ok(id) = receiver.recv() {
                let run = || -> Result<()> {
                    let plan = {
                        let mut records = records.lock().unwrap();
                        let target = resource(&records[&id].plan.action);
                        if records.values().any(|other| {
                            other.id != id
                                && other.status == "needs_inspection"
                                && resource(&other.plan.action) == target
                        }) {
                            let operation = records.get_mut(&id).unwrap();
                            operation.status = "needs_inspection".into();
                            operation.error = Some("同目标前序操作需核对，本操作未执行".into());
                            save_accepted(&store, &mut records, &id)?;
                            return Ok(());
                        }
                        let operation = records.get_mut(&id).context("操作记录不存在")?;
                        operation.status = "running".into();
                        let plan = operation.plan.clone();
                        save_accepted(&store, &mut records, &id)?;
                        plan
                    };
                    let progress_records = records.clone();
                    let progress_store = store.clone();
                    let progress_id = id.clone();
                    CHECKPOINT.with(|callback| {
                        *callback.borrow_mut() = Some(Box::new(move |stage| {
                            let mut records = progress_records.lock().unwrap();
                            let operation =
                                records.get_mut(&progress_id).context("操作记录不存在")?;
                            operation.stages.push(stage.into());
                            save_accepted(&progress_store, &mut records, &progress_id)
                        }))
                    });
                    let (output, result) = super::output::capture(|| {
                        progress("核对确认计划与实时资源")?;
                        ensure!(plan.expires >= now(), "确认计划已过期，请重新确认");
                        ensure!(
                            fingerprint(&registry, &plan.action)? == plan.fingerprint,
                            "资源已变化，请重新确认操作"
                        );
                        crate::execute_business(registry.clone(), plan.action, &plan.fingerprint)
                    });
                    CHECKPOINT.with(|callback| callback.borrow_mut().take());
                    super::REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut records = records.lock().unwrap();
                    let operation = records.get_mut(&id).unwrap();
                    operation.output = output;
                    if operation.status != "needs_inspection" {
                        operation.status = if result.is_ok() {
                            "succeeded"
                        } else {
                            "failed"
                        }
                        .into();
                        operation.error = result.err().map(|e| format!("{e:#}"));
                    }
                    save_accepted(&store, &mut records, &id)
                };
                if let Err(error) = run() {
                    super::log::error(&format!("操作 {id} 记录失败，需核对：{error:#}"));
                    // 内存保留待核对状态；磁盘故障恢复前也不允许继续派发同目标操作。
                }
            }
        });
        Ok((service, worker))
    }
    pub fn health(&self) -> Value {
        let records = self.records.lock().unwrap();
        json!({"queued": records.values().filter(|r|r.status=="queued").count(),
            "active":records.values().filter(|r|matches!(r.status.as_str(),"running"|"needs_inspection")).map(|r|json!({"id":r.id,"status":r.status,"stage":r.stages.last()})).collect::<Vec<_>>()})
    }
    pub fn handle(&self, method: &str, value: Value) -> Result<Value> {
        match method {
            "operation.prepare" => {
                let action: Action = serde_json::from_value(value)?;
                ensure!(
                    matches!(
                        action,
                        Action::Add { .. }
                            | Action::New { .. }
                            | Action::Ls { .. }
                            | Action::Open { .. }
                            | Action::Stop { .. }
                            | Action::Rm { .. }
                            | Action::Agent(_)
                    ),
                    "此入口不是后台业务操作"
                );
                let mut plan = Plan {
                    id: crate::agent::message_id(),
                    expires: now() + 300,
                    fingerprint: fingerprint(&self.registry, &action)?,
                    action,
                    branches: vec![],
                    common: None,
                    prompt: None,
                };
                if let Action::New {
                    project,
                    name,
                    expected_branch: None,
                    ..
                } = &plan.action
                {
                    let entry = registry::entry(&self.registry, project)?;
                    let project =
                        Project::discover_at(&registry::expand(&entry.path)?, &self.registry)?;
                    ensure!(
                        !project.state()?.contains_key(name),
                        "任务 {name} 已存在；请使用 open"
                    );
                    plan.branches = crate::existing_branches(&project.config()?, name)?;
                    plan.common = Some(project.common);
                }
                match &plan.action {
                    Action::Stop {
                        name, yes: false, ..
                    } => plan.prompt = Some(format!("关闭 {name} 窗口并终止其中的全部进程？")),
                    Action::Rm {
                        name: Some(name),
                        options,
                        ..
                    } if !options.yes => {
                        plan.prompt = Some(format!(
                            "移除任务 {name} 的窗口和 Worktree{}{}？",
                            if options.keep_branch {
                                "，保留分支"
                            } else {
                                "及本地分支"
                            },
                            if options.delete_ignored {
                                "（包含被忽略文件）"
                            } else {
                                ""
                            }
                        ))
                    }
                    _ => {}
                }
                if let Action::Rm {
                    project: alias,
                    name: Some(name),
                    options,
                    expected_task,
                } = &plan.action
                {
                    let entry = registry::entry(&self.registry, alias)?;
                    let repository =
                        Project::discover_at(&registry::expand(&entry.path)?, &self.registry)?;
                    if let Some(expected) = expected_task {
                        let (common, branch, path): (PathBuf, String, PathBuf) =
                            serde_json::from_str(expected)?;
                        let state = repository.state()?;
                        let task = crate::feature(&state, name)?;
                        ensure!(
                            repository.common == common
                                && task.branch == branch
                                && task.path == path,
                            "任务归属已变化，请取消并重新确认删除"
                        );
                    }
                    let (output, result) = super::output::capture(|| {
                        crate::remove(
                            &repository,
                            &repository.config()?,
                            &mut repository.state()?,
                            name,
                            options,
                            true,
                        )
                    });
                    result?;
                    if let Some(prompt) = &mut plan.prompt {
                        *prompt = format!("{}{}\n{prompt}", output.stdout, output.stderr);
                    }
                }
                let mut plans = self.plans.lock().unwrap();
                plans.retain(|_, p| p.expires >= now());
                ensure!(plans.len() < 1024, "待确认操作过多，请稍后重试");
                plans.insert(plan.id.clone(), plan.clone());
                Ok(serde_json::to_value(plan)?)
            }
            "operation.submit" => {
                let plan_id = value["plan"].as_str().context("缺少计划 ID")?;
                let request = value["request"].as_str().context("缺少请求 ID")?;
                let mut records = self.records.lock().unwrap();
                if let Some(record) = records.values().find(|r| r.request == request) {
                    ensure!(record.plan.id == plan_id, "同一请求 ID 不能对应不同计划");
                    return Ok(json!(record.id));
                }
                ensure!(
                    !records.values().any(|record| record.plan.id == plan_id),
                    "该计划已提交，请按原请求 ID 查询结果"
                );
                let mut plan = self
                    .plans
                    .lock()
                    .unwrap()
                    .get(plan_id)
                    .cloned()
                    .context("计划已失效，请重新确认")?;
                ensure!(plan.expires >= now(), "计划已过期，请重新确认");
                if plan.prompt.is_some() {
                    ensure!(value["confirmed"] == true, "请明确确认操作");
                }
                if !plan.branches.is_empty() {
                    let branch: crate::config::BranchStart =
                        serde_json::from_value(value["branch"].clone())
                            .context("同名分支需要明确选择")?;
                    ensure!(plan.branches.contains(&branch), "分支来源不属于确认计划");
                    if let Action::New {
                        expected_branch, ..
                    } = &mut plan.action
                    {
                        *expected_branch = Some(serde_json::to_string(&(&plan.common, branch))?);
                    }
                }
                match &mut plan.action {
                    Action::Stop { yes, .. } => *yes = true,
                    Action::Rm { options, .. } => options.yes = true,
                    _ => {}
                }
                let target = resource(&plan.action);
                ensure!(
                    !records
                        .values()
                        .any(|r| r.status == "needs_inspection"
                            && resource(&r.plan.action) == target),
                    "同目标有结果待核对的操作，请先检查 operation ls"
                );
                let id = crate::agent::message_id();
                records.insert(
                    id.clone(),
                    Operation {
                        id: id.clone(),
                        request: request.into(),
                        plan,
                        status: "queued".into(),
                        stages: vec![],
                        output: Output::default(),
                        error: None,
                    },
                );
                if let Err(error) = self.store.save(&records[&id]) {
                    records.remove(&id);
                    return Err(error).context("操作未接受：无法保存提交记录");
                }
                if let Err(error) = self.queue.try_send(id.clone()) {
                    let record = records.get_mut(&id).unwrap();
                    record.status = "failed".into();
                    record.error = Some(format!("未执行：操作队列不可用：{error}"));
                    save_accepted(&self.store, &mut records, &id)?;
                }
                Ok(json!(id))
            }
            "operation.get" => {
                let id = value.as_str().context("缺少操作 ID")?;
                let records = self.records.lock().unwrap();
                Ok(serde_json::to_value(
                    records
                        .get(id)
                        .or_else(|| records.values().find(|r| r.request == id))
                        .context("未找到操作")?,
                )?)
            }
            "operation.resolve" => {
                let id = value["id"].as_str().context("缺少操作 ID")?;
                ensure!(value["confirmed"] == true, "需先核对实际资源并明确确认");
                let mut records = self.records.lock().unwrap();
                let record = records.get_mut(id).context("操作不存在")?;
                ensure!(
                    record.status == "needs_inspection",
                    "只有待核对操作可解除标记"
                );
                let previous = record.clone();
                record.status = "resolved".into();
                record.error = Some("用户已核对资源并解除阻塞；此标记不表示原操作成功".into());
                if let Err(error) = self.store.save(record) {
                    records.insert(id.into(), previous);
                    return Err(error);
                }
                Ok(Value::Null)
            }
            "operation.list" => Ok(serde_json::to_value(&*self.records.lock().unwrap())?),
            _ => bail!("未知操作方法"),
        }
    }
}
fn resource(action: &Action) -> Option<&str> {
    match action {
        Action::New { project, .. }
        | Action::Open { project, .. }
        | Action::Stop { project, .. }
        | Action::Rm { project, .. } => Some(project),
        _ => None,
    }
}
pub(crate) fn fingerprint(registry: &Path, action: &Action) -> Result<Value> {
    if matches!(action, Action::Rm { name: None, .. }) {
        return Ok(serde_json::to_value(registry::load(registry)?)?);
    }
    let project = resource(action);
    if let Some(alias) = project {
        let entry = registry::entry(registry, alias)?;
        let path = registry::expand(&entry.path)?;
        let project = Project::discover_at(&path, registry)?;
        let config = project.config()?;
        let state = project.state()?;
        let name = match action {
            Action::New { name, .. } | Action::Open { name, .. } | Action::Stop { name, .. } => {
                Some(name.as_str())
            }
            Action::Rm { name, .. } => name.as_deref(),
            _ => None,
        };
        let task_path = name.and_then(|name| state.get(name)).map(|task| &task.path);
        let directory = task_path
            .or_else(|| {
                matches!(action, Action::Open { name, .. } if name == "main")
                    .then_some(&config.main)
            })
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            });
        let window = name
            .map(|name| crate::tmux::window(&config, &project, name).context("无法确认归属"))
            .transpose()?;
        if matches!(action, Action::Open { .. }) {
            // 打开只固定目标身份；启动程序写文件或 Git 提交不改变资源归属。
            // 执行时仍在项目锁内实时核对 Worktree 分支和路径归属。
            return Ok(json!({"path":path,"common":project.common,"config":config,
                "task":name.and_then(|name| state.get(name)),"directory":directory,"window":window}));
        }
        let files = task_path
            .filter(|path| path.is_dir())
            .map(|path| {
                crate::git::git(
                    path,
                    &[
                        "status",
                        "--porcelain",
                        "--untracked-files=all",
                        "--ignored=matching",
                    ],
                )
            })
            .transpose()?;
        let refs = crate::git::git(&config.main, &["show-ref"])?;
        let worktrees = crate::git::git(&config.main, &["worktree", "list", "--porcelain"])?;
        return Ok(
            json!({"path":path,"common":project.common,"config":config,"state":state,"refs":refs,"worktrees":worktrees,"files":files,"directory":directory,"window":window}),
        );
    }
    // 查询与 Agent 通信自行实时验证绑定；无需用整个登记表阻塞无关消息。
    if matches!(action, Action::Add { .. } | Action::Rm { .. }) {
        return Ok(serde_json::to_value(registry::load(registry)?)?);
    }
    Ok(Value::Null)
}

pub fn cli(registry: &Path, command: Command) -> Result<()> {
    let result: Value = match command {
        Command::Ls => super::call(registry, "operation.list", Value::Null)?,
        Command::Show { id } => super::call(registry, "operation.get", json!(id))?,
        Command::Resolve { id, yes } => {
            crate::confirm(
                "已核对真实资源、确认没有遗留执行进程，并解除此操作的阻塞标记？",
                yes,
            )?;
            super::call(
                registry,
                "operation.resolve",
                json!({"id":id,"confirmed":true}),
            )?
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
pub fn client(registry: &Path, action: Action) -> Result<()> {
    let output = execute(registry, action)?;
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    if let Some((path, target)) = output.terminal {
        crate::console::run(registry.to_owned(), path, Some(target))?;
    }
    Ok(())
}
pub fn execute(registry: &Path, mut action: Action) -> Result<Output> {
    if matches!(
        &action,
        Action::Ls { .. }
            | Action::Agent(crate::agent::Cli {
                command: crate::agent::Action::Status | crate::agent::Action::Read { .. }
            })
    ) {
        return super::call(registry, "query.execute", serde_json::to_value(action)?);
    }
    if let Action::Add { path, root, .. } = &mut action {
        *path = registry::expand(path)?;
        *root = root.as_deref().map(registry::expand).transpose()?;
    }
    let plan: Plan = super::call(registry, "operation.prepare", serde_json::to_value(action)?)?;
    let branch = if plan.branches.is_empty() {
        None
    } else {
        let base = match &plan.action {
            Action::New { base, .. } => base.clone(),
            _ => None,
        };
        Some(crate::choose_branch(&plan.branches, &base)?)
    };
    if let Some(prompt) = &plan.prompt {
        crate::confirm(prompt, false)?;
    }
    let request = crate::agent::message_id();
    let id: String = super::call(
        registry,
        "operation.submit",
        json!({"plan":plan.id,"request":request,"branch":branch,"confirmed":true}),
    )
    .with_context(|| {
        format!("提交结果待核对，请运行 twig operation show {request}；不要重新派发")
    })?;
    let mut shown = None;
    loop {
        let record: Operation = super::call(registry, "operation.get", json!(id))
            .with_context(|| format!("操作 {id} 的结果待核对；请运行 twig operation show {id}"))?;
        if std::env::var_os("TWIG_OPERATION_PROGRESS").is_some() {
            let stage = record
                .stages
                .last()
                .cloned()
                .unwrap_or_else(|| "已接受，等待执行".into());
            if shown.as_ref() != Some(&stage) {
                eprintln!("操作进度：{stage}（{id}）");
                shown = Some(stage);
            }
        }
        if !matches!(record.status.as_str(), "queued" | "running") {
            ensure!(
                record.status == "succeeded",
                "{}{}（操作 {id}）",
                record.output.stderr,
                record.error.as_deref().unwrap_or("结果需核对")
            );
            return Ok(record.output);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn finished(service: &Service, id: &str) -> Operation {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let record: Operation =
                serde_json::from_value(service.handle("operation.get", json!(id)).unwrap())
                    .unwrap();
            if !matches!(record.status.as_str(), "queued" | "running") {
                return record;
            }
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn legacy_new_operation_keeps_local_base_semantics() {
        let mut value = json!({"New":{"project":"demo","name":"task","base":null,
            "no_open":true,"agent":false,"expected_branch":null}});
        assert!(matches!(
            serde_json::from_value::<Action>(value.clone()).unwrap(),
            Action::New { no_fetch: true, .. }
        ));
        value["New"]["no_fetch"] = json!(false);
        assert!(matches!(
            serde_json::from_value::<Action>(value).unwrap(),
            Action::New {
                no_fetch: false,
                ..
            }
        ));
    }
    #[test]
    fn accepted_request_is_durable_and_retries_do_not_execute_twice() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("config.toml");
        let (service, worker) = Service::start(&registry, temp.path()).unwrap();
        let plan: Plan = serde_json::from_value(
            service
                .handle(
                    "operation.prepare",
                    serde_json::to_value(Action::Ls { project: None }).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let params = json!({"plan":plan.id,"request":"stable-request","confirmed":true});
        fs::remove_dir(&service.store.directory).unwrap();
        fs::write(&service.store.directory, "blocked").unwrap();
        assert!(service.handle("operation.submit", params.clone()).is_err());
        assert!(
            service
                .handle("operation.get", json!("stable-request"))
                .is_err()
        );
        assert!(service.records.lock().unwrap().is_empty());
        fs::remove_file(&service.store.directory).unwrap();
        super::super::private_directory(&service.store.directory).unwrap();
        let id = service.handle("operation.submit", params.clone()).unwrap();
        assert_eq!(id, service.handle("operation.submit", params).unwrap());
        assert!(
            service
                .handle(
                    "operation.submit",
                    json!({"plan":plan.id,"request":"different-request","confirmed":true})
                )
                .is_err()
        );
        assert_eq!(finished(&service, id.as_str().unwrap()).status, "succeeded");
        let (_, saved) = Store::open(temp.path()).unwrap();
        assert_eq!(saved.len(), 1);
        drop(service);
        worker.join().unwrap();
        let (service, worker) = Service::start(&registry, temp.path()).unwrap();
        assert_eq!(
            service
                .handle("operation.get", json!("stable-request"))
                .unwrap()["id"],
            id
        );
        drop(service);
        worker.join().unwrap();
    }
    #[test]
    fn interrupted_operation_is_not_replayed_and_requires_explicit_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let registry = temp.path().join("config.toml");
        let record = Operation {
            id: "operation".into(),
            request: "request".into(),
            plan: Plan {
                id: "plan".into(),
                expires: now() + 60,
                action: Action::Ls { project: None },
                fingerprint: Value::Null,
                branches: vec![],
                common: None,
                prompt: None,
            },
            status: "running".into(),
            stages: vec!["核对资源".into()],
            output: Output::default(),
            error: None,
        };
        fs::write(
            temp.path().join("operations.json"),
            serde_json::to_vec(&BTreeMap::from([("operation".to_owned(), record)])).unwrap(),
        )
        .unwrap();
        let (service, worker) = Service::start(&registry, temp.path()).unwrap();
        assert_eq!(finished(&service, "operation").status, "needs_inspection");
        assert!(
            service
                .handle(
                    "operation.resolve",
                    json!({"id":"operation","confirmed":false})
                )
                .is_err()
        );
        // 旧格式迁移后，原请求仍只返回原操作，不能再次执行。
        assert_eq!(
            service
                .handle(
                    "operation.submit",
                    json!({"plan":"plan","request":"request"})
                )
                .unwrap(),
            "operation"
        );
        assert_eq!(finished(&service, "request").status, "needs_inspection");
        // 解除标记保存失败时，不得在内存提前放开同目标操作。
        fs::remove_file(service.store.path("operation").unwrap()).unwrap();
        fs::create_dir(service.store.path("operation").unwrap()).unwrap();
        assert!(
            service
                .handle(
                    "operation.resolve",
                    json!({"id":"operation","confirmed":true})
                )
                .is_err()
        );
        assert_eq!(finished(&service, "operation").status, "needs_inspection");
        fs::remove_dir(service.store.path("operation").unwrap()).unwrap();
        service
            .handle(
                "operation.resolve",
                json!({"id":"operation","confirmed":true}),
            )
            .unwrap();
        assert_eq!(finished(&service, "operation").status, "resolved");
        drop(service);
        worker.join().unwrap();
    }

    #[test]
    fn accepted_save_failures_require_inspection_and_block_new_requests() {
        let temp = tempfile::tempdir().unwrap();
        let (service, worker) =
            Service::start(&temp.path().join("config.toml"), temp.path()).unwrap();
        let plan: Plan = serde_json::from_value(
            service
                .handle(
                    "operation.prepare",
                    serde_json::to_value(Action::Ls { project: None }).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let record = Operation {
            id: "accepted".into(),
            request: "original".into(),
            plan: plan.clone(),
            status: "queued".into(),
            stages: vec![],
            output: Output::default(),
            error: None,
        };
        service
            .records
            .lock()
            .unwrap()
            .insert(record.id.clone(), record);
        service
            .store
            .save(&service.records.lock().unwrap()["accepted"])
            .unwrap();
        fs::remove_file(service.store.path("accepted").unwrap()).unwrap();
        fs::create_dir(service.store.path("accepted").unwrap()).unwrap();
        service.queue.send("accepted".into()).unwrap();
        assert_eq!(finished(&service, "accepted").status, "needs_inspection");
        // 进度与最终结果写入失败使用同一失败语义，不保留运行中或宣称成功。
        for status in ["running", "succeeded", "failed"] {
            let mut records = service.records.lock().unwrap();
            records.get_mut("accepted").unwrap().status = status.into();
            assert!(save_accepted(&service.store, &mut records, "accepted").is_err());
            assert_eq!(records["accepted"].status, "needs_inspection");
            assert!(
                records["accepted"]
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("保存失败")
            );
        }
        let mut other = plan;
        other.id = "other-plan".into();
        service
            .plans
            .lock()
            .unwrap()
            .insert(other.id.clone(), other.clone());
        assert!(
            service
                .handle("operation.submit", json!({"plan":other.id,"request":"new"}))
                .unwrap_err()
                .to_string()
                .contains("待核对")
        );
        fs::remove_dir(service.store.path("accepted").unwrap()).unwrap();
        // 已排队的同目标操作也要在执行前检查，不能只拦截新提交。
        let mut queued = service.records.lock().unwrap()["accepted"].clone();
        queued.id = "queued-next".into();
        queued.request = "queued-request".into();
        queued.status = "queued".into();
        service
            .records
            .lock()
            .unwrap()
            .insert(queued.id.clone(), queued);
        service.queue.send("queued-next".into()).unwrap();
        let blocked = finished(&service, "queued-next");
        assert_eq!(blocked.status, "needs_inspection");
        assert!(blocked.error.unwrap().contains("本操作未执行"));
        service
            .handle(
                "operation.resolve",
                json!({"id":"accepted","confirmed":true}),
            )
            .unwrap();
        drop(service);
        worker.join().unwrap();
    }
}
