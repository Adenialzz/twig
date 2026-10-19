use anyhow::{Context, Result, bail};
use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Output},
};

pub fn run(program: &str, args: &[&OsStr], cwd: Option<&Path>) -> Result<Output> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    crate::console::query::output(&mut command)
        .with_context(|| format!("无法运行 {program}，请检查是否已安装"))
}

pub fn checked(program: &str, args: &[&OsStr], cwd: Option<&Path>) -> Result<String> {
    let output = run(program, args, cwd)?;
    if !output.status.success() {
        bail!(
            "{program} 执行失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches('\n')
        .to_owned())
}

pub fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    checked(
        "git",
        &args.iter().map(OsStr::new).collect::<Vec<_>>(),
        Some(cwd),
    )
}

pub fn exists(cwd: &Path, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let result = run(
        "git",
        &[
            "show-ref".as_ref(),
            "--verify".as_ref(),
            "--quiet".as_ref(),
            reference.as_ref(),
        ],
        Some(cwd),
    )?;
    match result.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("无法查询分支 {branch}"),
    }
}

pub fn merged(cwd: &Path, branch: &str, target: &str) -> Result<bool> {
    let result = run(
        "git",
        &[
            "merge-base".as_ref(),
            "--is-ancestor".as_ref(),
            branch.as_ref(),
            target.as_ref(),
        ],
        Some(cwd),
    )?;
    match result.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("无法比较 {branch} 与 {target}"),
    }
}

pub fn worktrees(cwd: &Path) -> Result<Vec<(std::path::PathBuf, String)>> {
    let output = run(
        "git",
        &[
            "worktree".as_ref(),
            "list".as_ref(),
            "--porcelain".as_ref(),
            "-z".as_ref(),
        ],
        Some(cwd),
    )?;
    anyhow::ensure!(output.status.success(), "无法查询 Worktree");
    let data = String::from_utf8(output.stdout).context("暂不支持非 UTF-8 路径")?;
    let mut result = Vec::new();
    let mut path = None;
    let mut branch = String::new();
    for field in data.split('\0') {
        if let Some(value) = field.strip_prefix("worktree ") {
            path = Some(value.into());
        }
        if let Some(value) = field.strip_prefix("branch refs/heads/") {
            branch = value.to_owned();
        }
        if field.is_empty()
            && let Some(p) = path.take()
        {
            result.push((p, std::mem::take(&mut branch)));
        }
    }
    Ok(result)
}

pub fn ignored(path: &Path) -> Result<Vec<String>> {
    let text = git(
        path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    )?;
    let mut paths: Vec<_> = text
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    Ok(paths)
}
