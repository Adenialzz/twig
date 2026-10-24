use crate::{config, git::git};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<crate::startup::Layout>,
    #[serde(default)]
    pub projects: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_last_used: BTreeMap<String, u64>,
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
        Ok(())
    }
}

pub fn entry(path: &Path, name: &str) -> Result<Entry> {
    load(path)?
        .projects
        .remove(name)
        .with_context(|| format!("未登记项目 {name}；请运行 twig ls"))
}

/// 仅成功创建任务或进入工作台时记录使用时间，读取操作不写入。
pub fn record_use(path: &Path, name: &str, task: &str) -> Result<()> {
    let _lock = config::Lock::acquire(path.with_extension("lock"))?;
    let mut registry = load(path)?;
    let entry = registry
        .projects
        .get_mut(name)
        .context("使用记录对应的项目已取消登记")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis()
        .try_into()?;
    entry.last_used = Some(now);
    entry.task_last_used.insert(task.to_owned(), now);
    save(path, &registry).context("任务操作已成功，但保存项目和任务使用时间失败")
}

pub fn recent_projects(path: &Path) -> Result<Vec<(String, Entry)>> {
    let mut projects: Vec<_> = load(path)?.projects.into_iter().collect();
    projects.sort_by(|a, b| b.1.last_used.cmp(&a.1.last_used).then(a.0.cmp(&b.0)));
    Ok(projects)
}

pub fn list(path: &Path) -> Result<()> {
    for (name, entry) in load(path)?.projects {
        println!("{name}\t{}", entry.path.display());
    }
    Ok(())
}

pub fn remove(path: &Path, name: &str) -> Result<()> {
    // 先读取，缺失登记表时不创建文件。
    entry(path, name)?;
    let _lock = config::Lock::acquire(path.with_extension("lock"))?;
    let mut registry = load(path)?;
    ensure!(
        registry.projects.remove(name).is_some(),
        "未登记项目 {name}"
    );
    save(path, &registry)?;
    println!("已取消登记 {name}；仓库、任务和项目状态保留");
    Ok(())
}

pub fn add(
    registry_path: &Path,
    path: &Path,
    name: String,
    base: Option<String>,
    target: Option<String>,
    root: Option<PathBuf>,
) -> Result<String> {
    config::name(&name)?;
    let root = root.map(|path| expand(&path)).transpose()?;
    let path = expand(path)?.canonicalize().context("仓库目录不存在")?;
    ensure!(
        git(&path, &["rev-parse", "--is-bare-repository"])? == "false",
        "暂不支持裸仓库"
    );
    let main = crate::git::worktrees(&path)?
        .first()
        .context("未找到主工作目录")?
        .0
        .canonicalize()?;
    std::env::set_current_dir(&main)?;
    let project = config::Project::discover_at(&main, registry_path)?;
    let _lock = project.lock()?;
    initialize(registry_path, &name, || {
        crate::init(&project, base, target, root)
    })
}

fn save(path: &Path, registry: &Registry) -> Result<()> {
    let temp = path.with_extension("toml.tmp");
    fs::write(&temp, toml::to_string_pretty(&registry)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

/// 持有登记表锁，先检查冲突，再初始化；只有初始化成功才写入新登记。
fn initialize(
    path: &Path,
    requested: &str,
    initialize_project: impl FnOnce() -> Result<()>,
) -> Result<String> {
    let main = crate::git::worktrees(&std::env::current_dir()?)?
        .first()
        .context("未找到主工作目录")?
        .0
        .canonicalize()?;
    fs::create_dir_all(path.parent().context("配置路径缺少父目录")?)?;
    let _lock = config::Lock::acquire(path.with_extension("lock"))?;
    let mut registry = load(path)?;
    let mut existing = None;
    for (name, entry) in &registry.projects {
        let entry_path = expand(&entry.path)?;
        let same =
            entry_path == main || (entry_path.try_exists()? && entry_path.canonicalize()? == main);
        if requested == name {
            ensure!(same, "项目别名 {name} 已指向其他路径；请使用其他项目别名");
        }
        if same && (existing.is_none() || requested == name) {
            existing = Some(name.clone());
        }
    }
    let name = existing.clone().unwrap_or_else(|| requested.to_owned());
    initialize_project()?;
    if existing.is_none() {
        registry.projects.insert(
            name.clone(),
            Entry {
                path: main,
                last_used: None,
                task_last_used: BTreeMap::new(),
            },
        );
        save(path, &registry).context("项目已初始化，但全局登记保存失败；请重试 twig add")?;
    }
    println!("项目别名：{name}（{}）", path.display());
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_usage_persists_without_changing_paths_or_startup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("custom.toml");
        assert!(recent_projects(&path).unwrap().is_empty());
        assert!(!path.exists());
        fs::write(
            &path,
            r#"
[tmux]
panes = [{ command = ["echo", "hello"], focus = true }]
[projects.alpha]
path = "/alpha"
[projects.beta]
path = "/beta"
last_used = 1
[projects.gamma]
path = "/gamma"
last_used = 1
"#,
        )
        .unwrap();
        let before = fs::read(&path).unwrap();
        let names = || {
            recent_projects(&path)
                .unwrap()
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(), ["beta", "gamma", "alpha"]);
        assert_eq!(fs::read(&path).unwrap(), before);
        record_use(&path, "alpha", "feat/auth").unwrap();
        assert_eq!(names(), ["alpha", "beta", "gamma"]);
        let registry = load(&path).unwrap();
        assert_eq!(registry.projects["alpha"].path, Path::new("/alpha"));
        assert!(registry.projects["alpha"].last_used.unwrap() > 1);
        assert_eq!(
            registry.projects["alpha"].task_last_used["feat/auth"],
            registry.projects["alpha"].last_used.unwrap()
        );
        assert_eq!(
            registry.tmux.unwrap().panes[0].command.as_deref().unwrap(),
            ["echo", "hello"]
        );
        let saved = fs::read(&path).unwrap();
        assert!(record_use(&path, "missing", "main").is_err());
        assert_eq!(fs::read(&path).unwrap(), saved);
    }
}
