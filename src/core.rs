//! 普通任务在调用进程执行；后台仅承载需要持续通信的受管操作。
use crate::{
    Action,
    config::Project,
    daemon::{
        self,
        operations::{self, Command, Operation, store::Store},
        output::Output,
    },
    registry,
};
use anyhow::{Context, Result, ensure};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

type Records = BTreeMap<String, Operation>;
fn directory(registry: &Path) -> Result<PathBuf> {
    let paths = daemon::Paths::new(registry)?;
    Ok(paths
        .dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("operations")
        .join(crate::config::repository_id(&paths.registry)))
}
pub fn execution_lock(registry: &Path) -> Result<File> {
    let dir = directory(registry)?;
    fs::create_dir_all(dir.parent().unwrap())?;
    daemon::private_directory(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("execution.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
        "无法锁定任务执行：{}",
        std::io::Error::last_os_error()
    );
    Ok(lock)
}
fn pending(records: &Records, action: &Action) -> Result<()> {
    ensure!(
        !records.values().any(|record| matches!(
            record.status.as_str(),
            "queued" | "running" | "needs_inspection"
        ) && operations::resource(&record.plan.action)
            == operations::resource(action)),
        "同目标有结果待核对或正在执行的操作，请检查 twig operation ls"
    );
    Ok(())
}
pub fn check_pending(registry: &Path, action: &Action) -> Result<()> {
    pending(&Store::read(&directory(registry)?)?, action)
}
fn managed(registry: &Path, action: &Action) -> Result<bool> {
    match action {
        Action::Agent(_) | Action::New { agent: true, .. } => Ok(true),
        Action::Open { project, name }
        | Action::Stop { project, name, .. }
        | Action::Rm {
            project,
            name: Some(name),
            ..
        } if name != "main" => {
            let entry = registry::entry(registry, project)?;
            let repository = Project::discover_at(&registry::expand(&entry.path)?, registry)?;
            Ok(repository
                .state()?
                .get(name)
                .is_some_and(|task| task.agent_thread.is_some()))
        }
        _ => Ok(false),
    }
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
    if managed(registry, &action)? {
        return operations::execute_remote(registry, action);
    }
    if matches!(action, Action::Ls { .. }) {
        let (output, result) = daemon::output::capture(|| {
            crate::execute_business(registry.to_owned(), action, &Value::Null)
        });
        result?;
        return Ok(output);
    }
    if let Action::Add { path, root, .. } = &mut action {
        *path = registry::expand(path)?;
        *root = root.as_deref().map(registry::expand).transpose()?;
    }
    let mut plan = operations::prepare(registry, action)?;
    if !plan.branches.is_empty() {
        let base = match &plan.action {
            Action::New { base, .. } => base.clone(),
            _ => None,
        };
        let branch = crate::choose_branch(&plan.branches, &base)?;
        if let Action::New {
            expected_branch, ..
        } = &mut plan.action
        {
            *expected_branch = Some(serde_json::to_string(&(&plan.common, branch))?);
        }
    }
    if let Some(prompt) = &plan.prompt {
        crate::confirm(prompt, false)?;
    }
    match &mut plan.action {
        Action::Stop { yes, .. } => *yes = true,
        Action::Rm { options, .. } => options.yes = true,
        _ => {}
    }
    // 用户确认期间不持锁；所有资源写入都在相同执行锁内重新校验。
    let _lock = execution_lock(registry)?;
    ensure!(
        !managed(registry, &plan.action)?,
        "任务已变为受管任务，请重新执行并连接后台"
    );
    let (store, mut records) = Store::open(&directory(registry)?)?;
    for id in recover(&mut records) {
        store.save(&records[&id])?;
    }
    pending(&records, &plan.action)?;
    pending(
        &Store::read(&daemon::Paths::new(registry)?.dir)?,
        &plan.action,
    )?;
    ensure!(
        plan.expires >= operations::now(),
        "确认计划已过期，请重新确认"
    );
    ensure!(
        operations::fingerprint(registry, &plan.action)? == plan.fingerprint,
        "资源已变化，请重新确认操作"
    );
    let id = format!("local-{}", crate::agent::message_id());
    records.insert(
        id.clone(),
        Operation {
            id: id.clone(),
            request: id.clone(),
            plan: plan.clone(),
            status: "running".into(),
            stages: vec![],
            output: Output::default(),
            error: None,
        },
    );
    store.save(&records[&id])?;
    eprintln!("操作 {id}");
    let records = std::rc::Rc::new(std::cell::RefCell::new(records));
    let progress_records = records.clone();
    let progress_store = store.clone();
    let progress_id = id.clone();
    let persistence_failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let progress_failed = persistence_failed.clone();
    operations::checkpoint(Some(Box::new(move |stage| {
        let mut records = progress_records.borrow_mut();
        records
            .get_mut(&progress_id)
            .unwrap()
            .stages
            .push(stage.into());
        let result = progress_store.save(&records[&progress_id]);
        if result.is_err() {
            progress_failed.set(true);
        }
        if std::env::var_os("TWIG_OPERATION_PROGRESS").is_some() {
            eprintln!("操作进度：{stage}（{progress_id}）");
        }
        result
    })));
    let (output, result) = daemon::output::capture(|| {
        crate::execute_business(registry.to_owned(), plan.action, &plan.fingerprint)
    });
    operations::checkpoint(None);
    let mut records = records.borrow_mut();
    let record = records.get_mut(&id).unwrap();
    record.output = output.clone();
    record.status = if persistence_failed.get() {
        "needs_inspection"
    } else if result.is_ok() {
        "succeeded"
    } else {
        "failed"
    }
    .into();
    record.error = result.as_ref().err().map(|error| format!("{error:#}"));
    store
        .save(record)
        .with_context(|| format!("操作 {id} 结果保存失败，请核对资源，不要重跑"))?;
    daemon::REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    result.with_context(|| format!("操作 {id} 失败；twig operation show {id} 查看详情"))?;
    Ok(output)
}
fn recover(records: &mut Records) -> Vec<String> {
    let mut changed = Vec::new();
    for record in records
        .values_mut()
        .filter(|r| matches!(r.status.as_str(), "running" | "queued"))
    {
        changed.push(record.id.clone());
        record.status = "needs_inspection".into();
        record.error = Some("执行进程已退出，结果需核对；不会自动重跑".into());
    }
    changed
}
pub fn operation_cli(registry: &Path, command: Command) -> Result<()> {
    let root = directory(registry)?;
    let mut local = Store::read(&root)?;
    // 查询不创建文件；只有能取得执行锁时，才把中断记录解释为待核对。
    if !local.is_empty() {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory(registry)?.join("execution.lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            local = Store::read(&root)?;
            recover(&mut local);
        }
    }
    let result = match command {
        Command::Ls => {
            let mut records = Store::read(&daemon::Paths::new(registry)?.dir)?;
            records.extend(local);
            serde_json::to_value(records)?
        }
        Command::Show { id } if local.contains_key(&id) => serde_json::to_value(&local[&id])?,
        Command::Show { id } => {
            let records = Store::read(&daemon::Paths::new(registry)?.dir)?;
            serde_json::to_value(
                records
                    .get(&id)
                    .or_else(|| records.values().find(|r| r.request == id))
                    .context("未找到操作")?,
            )?
        }
        Command::Resolve { id, yes } if local.contains_key(&id) => {
            crate::confirm("已核对真实资源和遗留进程，并解除操作阻塞？", yes)?;
            let _lock = execution_lock(registry)?;
            let (store, mut records) = Store::open(&root)?;
            recover(&mut records);
            let record = records.get_mut(&id).context("操作不存在")?;
            ensure!(
                record.status == "needs_inspection",
                "只有待核对操作可解除标记"
            );
            record.status = "resolved".into();
            record.error = Some("用户已核对资源并解除阻塞；不表示原操作成功".into());
            store.save(record)?;
            Value::Null
        }
        command => return operations::remote_cli(registry, command),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
pub fn query<T: DeserializeOwned>(registry: &Path, method: &str, value: Value) -> Result<T> {
    Ok(serde_json::from_value(daemon::queries::handle(
        registry, method, value,
    )?)?)
}
pub fn record_use(registry: &Path, project: &str, task: &str) -> Result<()> {
    let entry = registry::entry(registry, project)?;
    let repository = Project::discover_at(&registry::expand(&entry.path)?, registry)?;
    ensure!(
        crate::tmux::window(&repository.config()?, &repository, task)?.is_some(),
        "窗口已失效"
    );
    registry::record_use(registry, project, task)
}
