use crate::git::git;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub version: u32,
    pub main: PathBuf,
    pub root: PathBuf,
    pub base: String,
    pub target: String,
    pub session: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Feature {
    pub branch: String,
    pub base: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_start: Option<BranchStart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_thread: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BranchStart {
    pub reference: String,
    pub commit: String,
}
impl BranchStart {
    pub fn label(&self) -> &str {
        self.reference
            .strip_prefix("refs/heads/")
            .or_else(|| self.reference.strip_prefix("refs/remotes/"))
            .unwrap_or(&self.reference)
    }
}

pub type State = BTreeMap<String, Feature>;

#[derive(Clone)]
pub struct Project {
    pub common: PathBuf,
    pub dir: PathBuf,
    pub registry: PathBuf,
}

impl Project {
    pub fn discover_at(cwd: &Path, registry: &Path) -> Result<Self> {
        let common = PathBuf::from(git(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()?;
        Ok(Self {
            registry: registry.to_owned(),
            dir: registry
                .parent()
                .context("登记表缺少父目录")?
                .join("projects")
                .join(repository_id(&common)),
            common,
        })
    }
    pub fn lock(&self) -> Result<Lock> {
        fs::create_dir_all(&self.dir)?;
        Lock::acquire(self.dir.join("lock"))
    }
    pub fn config(&self) -> Result<Config> {
        let config: Config = serde_json::from_slice(
            &fs::read(self.dir.join("config.json"))
                .context("请先执行 twig add <项目名> --path <仓库路径>")?,
        )?;
        if config.version != 1 {
            bail!("不支持配置版本 {}", config.version);
        }
        let common = PathBuf::from(git(
            &config.main,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()?;
        ensure!(common == self.common, "项目配置的仓库归属不一致");
        Ok(config)
    }
    pub fn state(&self) -> Result<State> {
        let path = self.dir.join("state.json");
        if !path.exists() {
            return Ok(State::new());
        }
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }
    pub fn save(&self, filename: &str, value: &impl Serialize) -> Result<()> {
        let path = self.dir.join(filename);
        let temp = self.dir.join(format!("{filename}.tmp"));
        fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
        fs::rename(temp, path)?;
        Ok(())
    }
}

pub struct Lock(PathBuf);
impl Lock {
    pub fn acquire(path: PathBuf) -> Result<Self> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "已锁定；若上次命令异常终止，确认没有 twig 运行后删除 {}",
                    path.display()
                )
            })?;
        let lock = Self(path);
        use std::io::Write;
        writeln!(file, "{}", std::process::id())?;
        Ok(lock)
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("名称须为 1–64 位 ASCII 字母、数字、横线或下划线，且以字母或数字开头");
    }
    Ok(())
}

pub fn task_name(value: &str) -> Result<()> {
    if value.len() > 64 || value.split('/').any(|part| name(part).is_err()) {
        bail!(
            "任务名称须为 1–64 位 ASCII 字母、数字、横线、下划线或 /；每段以字母或数字开头，不允许空段"
        );
    }
    Ok(())
}

pub fn repository_id(path: &Path) -> String {
    let hash = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

pub fn session(path: &Path) -> String {
    format!("twig-{}", repository_id(path))
}
