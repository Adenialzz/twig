use crate::{config, git::git};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use clap_complete::ArgValueCompleter;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Subcommand)]
pub enum Action {
    /// 登记项目；仅保存配置，不联网、不修改已有 Git remote
    Add {
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        path: PathBuf,
        #[arg(long, default_value = "main")]
        main: String,
        #[arg(long, default_value = "origin")]
        remote: String,
    },
    /// 列出已登记项目
    List,
    /// 显示项目 TOML 配置
    Show {
        #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
        name: String,
    },
    /// 只取消登记，保留本地仓库和任务
    Remove {
        #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
        name: String,
    },
    /// 管理登记表中的多个 remote；不覆盖已有仓库配置
    Remote {
        #[command(subcommand)]
        command: RemoteAction,
    },
}

#[derive(Subcommand)]
pub enum RemoteAction {
    Add {
        #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
        project: String,
        name: String,
        url: String,
    },
    Remove {
        #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
        project: String,
        #[arg(add = ArgValueCompleter::new(crate::completion::remotes))]
        name: String,
    },
    /// 设置首次 clone 使用的 remote
    Default {
        #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
        project: String,
        #[arg(add = ArgValueCompleter::new(crate::completion::remotes))]
        name: String,
    },
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default)]
    pub projects: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: PathBuf,
    pub main: String,
    pub default_remote: String,
    pub remotes: BTreeMap<String, String>,
}

pub fn expand(path: &Path) -> Result<PathBuf> {
    if path == Path::new("~") || path.starts_with("~/") {
        let home = std::env::var_os("HOME").context("HOME 未设置，无法展开 ~")?;
        return Ok(PathBuf::from(home).join(path.strip_prefix("~")?));
    }
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub fn location(custom: Option<PathBuf>) -> Result<PathBuf> {
    expand(&custom.unwrap_or_else(|| PathBuf::from("~/.twig/config.toml")))
}

pub fn load(path: &Path) -> Result<Registry> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => return Err(e).with_context(|| format!("无法读取 {}", path.display())),
    };
    let registry: Registry =
        toml::from_str(&text).with_context(|| format!("配置格式错误：{}", path.display()))?;
    for (name, entry) in &registry.projects {
        config::name(name)?;
        entry.validate()?;
    }
    Ok(registry)
}

impl Entry {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.path.is_absolute() || self.path.starts_with("~"),
            "配置中的项目路径必须是绝对路径或以 ~/ 开头"
        );
        git(
            &std::env::current_dir()?,
            &["check-ref-format", &format!("refs/heads/{}", self.main)],
        )
        .context("主分支名无效")?;
        ensure!(!self.main.starts_with('-'), "主分支名不能以 - 开头");
        ensure!(
            self.remotes.contains_key(&self.default_remote),
            "default_remote 不在 remotes 中：{}",
            self.default_remote
        );
        for (name, url) in &self.remotes {
            config::name(name)?;
            ensure!(!url.trim().is_empty(), "remote {name} 的 URL 不能为空");
        }
        Ok(())
    }
}

pub fn entry(path: &Path, name: &str) -> Result<Entry> {
    load(path)?
        .projects
        .remove(name)
        .with_context(|| format!("未登记项目 {name}；请运行 twig config list"))
}

pub fn execute(path: &Path, action: Action) -> Result<()> {
    // 查询不创建目录或锁文件。
    match &action {
        Action::List => {
            for (name, entry) in load(path)?.projects {
                println!(
                    "{name}\t{}\t{}\t{}",
                    entry.path.display(),
                    entry.main,
                    entry.default_remote
                );
            }
            return Ok(());
        }
        Action::Show { name } => {
            let mut projects = BTreeMap::new();
            projects.insert(name.clone(), entry(path, name)?);
            print!("{}", toml::to_string_pretty(&Registry { projects })?);
            return Ok(());
        }
        _ => {}
    }
    fs::create_dir_all(path.parent().context("配置路径缺少父目录")?)?;
    let _lock = config::Lock::acquire(path.with_extension("lock"))?;
    let mut registry = load(path)?;
    match action {
        Action::Add {
            name,
            url,
            path: repo_path,
            main,
            remote,
        } => {
            config::name(&name)?;
            ensure!(
                !registry.projects.contains_key(&name),
                "项目 {name} 已登记；请编辑配置或先取消登记"
            );
            let entry = Entry {
                path: expand(&repo_path)?,
                main,
                default_remote: remote.clone(),
                remotes: BTreeMap::from([(remote, url)]),
            };
            entry.validate()?;
            registry.projects.insert(name, entry);
        }
        Action::Remove { name } => {
            ensure!(
                registry.projects.remove(&name).is_some(),
                "未登记项目 {name}"
            );
        }
        Action::Remote { command } => {
            let project = match &command {
                RemoteAction::Add { project, .. }
                | RemoteAction::Remove { project, .. }
                | RemoteAction::Default { project, .. } => project,
            };
            let entry = registry
                .projects
                .get_mut(project)
                .with_context(|| format!("未登记项目 {project}"))?;
            match command {
                RemoteAction::Add { name, url, .. } => {
                    ensure!(
                        !entry.remotes.contains_key(&name),
                        "remote {name} 已登记；请编辑配置或先移除"
                    );
                    entry.remotes.insert(name, url);
                }
                RemoteAction::Remove { name, .. } => {
                    ensure!(
                        name != entry.default_remote,
                        "请先设置其他 default remote，再移除 {name}"
                    );
                    ensure!(
                        entry.remotes.remove(&name).is_some(),
                        "remote {name} 不存在"
                    );
                }
                RemoteAction::Default { name, .. } => entry.default_remote = name,
            }
            entry.validate()?;
        }
        Action::List | Action::Show { .. } => unreachable!(),
    }
    let temp = path.with_extension("toml.tmp");
    fs::write(&temp, toml::to_string_pretty(&registry)?)?;
    fs::rename(temp, path)?;
    println!("登记表已保存：{}", path.display());
    Ok(())
}

pub fn clone(entry: &Entry) -> Result<()> {
    let path = expand(&entry.path)?;
    let cwd = std::env::current_dir()?;
    let url = &entry.remotes[&entry.default_remote];
    // 只接续归属完全匹配的仓库，绝不覆盖已有目录或重写 remote。
    if fs::symlink_metadata(&path).is_ok() {
        let root = git(&path, &["rev-parse", "--show-toplevel"])
            .context("目标路径已存在且不是可接续的仓库，未覆盖")?;
        ensure!(
            Path::new(&root).canonicalize()? == path.canonicalize()?,
            "目标路径不是仓库根目录，未覆盖"
        );
        let existing = git(
            &path,
            &[
                "config",
                "--get",
                &format!("remote.{}.url", entry.default_remote),
            ],
        )?;
        ensure!(
            existing == *url,
            "默认 remote URL 与登记不符，未覆盖：{existing}"
        );
    } else {
        fs::create_dir_all(path.parent().context("仓库路径缺少父目录")?)?;
        // 保留 Git 的终端交互和进度输出，认证仍由 Git/SSH/credential helper 处理。
        let status = Command::new("git")
            .current_dir(&cwd)
            .args([
                "clone",
                "--origin",
                &entry.default_remote,
                "--branch",
                &entry.main,
                "--",
                url,
                path.to_str().context("暂不支持非 UTF-8 路径")?,
            ])
            .status()
            .context("无法运行 git clone")?;
        ensure!(
            status.success(),
            "git clone 失败；请检查上方 Git 错误和目标目录后重试"
        );
    }
    // 添加任何 remote 之前检查所有已有配置，避免发现冲突前做部分修改。
    let existing = git(&path, &["remote"])?;
    for (name, url) in &entry.remotes {
        if existing.lines().any(|r| r == name) {
            let actual = git(&path, &["config", "--get", &format!("remote.{name}.url")])?;
            ensure!(
                actual == *url,
                "remote {name} URL 与登记不符，未覆盖：{actual}"
            );
        }
    }
    ensure!(
        crate::git::exists(&path, &entry.main)?,
        "仓库缺少本地主分支 {}，请手动检查",
        entry.main
    );
    // 在增加 remote 前检查已有 twig 配置是否与登记表冲突。
    std::env::set_current_dir(&path)?;
    let project = config::Project::discover()?;
    let _lock = project.lock()?;
    if project.dir.join("config.json").exists() {
        let config = project.config()?;
        ensure!(
            config.base == entry.main && config.target == entry.main,
            "登记主分支与已有 twig 配置冲突，未覆盖"
        );
    }
    for (name, url) in &entry.remotes {
        if !existing.lines().any(|r| r == name) {
            git(&path, &["remote", "add", "--", name, url])?;
        }
    }
    crate::init(
        &project,
        Some(entry.main.clone()),
        Some(entry.main.clone()),
        None,
    )
    .context("仓库和 remote 已保留，修复后可重试 twig clone")?;
    Ok(())
}
