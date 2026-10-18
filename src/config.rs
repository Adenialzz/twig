use crate::git::git;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
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
}

pub type State = BTreeMap<String, Feature>;

pub struct Project {
    pub common: PathBuf,
    pub dir: PathBuf,
}

impl Project {
    pub fn discover() -> Result<Self> {
        Self::discover_at(&std::env::current_dir()?)
    }
    pub fn discover_at(cwd: &Path) -> Result<Self> {
        let common = PathBuf::from(git(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()?;
        Ok(Self {
            dir: common.join("twig"),
            common,
        })
    }
    pub fn lock(&self) -> Result<Lock> {
        fs::create_dir_all(&self.dir)?;
        Lock::acquire(self.dir.join("lock"))
    }
    pub fn config(&self) -> Result<Config> {
        let config: Config = serde_json::from_slice(
            &fs::read(self.dir.join("config.json")).context("请先执行 twig init")?,
        )?;
        if config.version != 1 {
            bail!("不支持配置版本 {}", config.version);
        }
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

pub fn session(path: &Path) -> String {
    let hash = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    format!("twig-{hash:016x}")
}
