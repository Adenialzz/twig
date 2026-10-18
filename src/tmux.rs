use crate::{
    config::{Config, Project},
    git::{checked, run},
};
use anyhow::{Result, bail, ensure};
use std::{ffi::OsStr, path::Path, process::Command};

pub fn tmux(args: &[&str]) -> Result<String> {
    checked(
        "tmux",
        &args.iter().map(OsStr::new).collect::<Vec<_>>(),
        None,
    )
}

pub fn session_exists(config: &Config, project: &Project) -> Result<bool> {
    let target = format!("={}", config.session);
    let result = run(
        "tmux",
        &["has-session".as_ref(), "-t".as_ref(), target.as_ref()],
        None,
    )?;
    if !result.status.success() {
        let error = String::from_utf8_lossy(&result.stderr);
        if error.contains("can't find session")
            || error.contains("no server running")
            || error.contains("No such file or directory")
        {
            return Ok(false);
        }
        bail!("无法查询 tmux：{}", error.trim());
    }
    let owner = tmux(&[
        "show-options",
        "-qv",
        "-t",
        &format!("{}:", config.session),
        "@twig-repo",
    ])?;
    ensure!(
        owner == project.common.to_string_lossy(),
        "同名 tmux Session 不属于本项目：{}",
        config.session
    );
    Ok(true)
}

pub fn ensure_session(config: &Config, project: &Project) -> Result<()> {
    if !session_exists(config, project)? {
        tmux(&[
            "new-session",
            "-d",
            "-s",
            &config.session,
            "-n",
            "main",
            "-c",
            config.main.to_str().unwrap(),
            ";",
            "set-option",
            "-t",
            &format!("{}:", config.session),
            "@twig-repo",
            project.common.to_str().unwrap(),
        ])?;
    }
    Ok(())
}

pub fn window(config: &Config, project: &Project, name: &str) -> Result<Option<String>> {
    if !session_exists(config, project)? {
        return Ok(None);
    }
    let output = tmux(&[
        "list-windows",
        "-t",
        &format!("={}", config.session),
        "-F",
        "#{window_id}\t#{window_name}\t#{@twig-feature}",
    ])?;
    let mut found = None;
    let mut collision = false;
    for line in output.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() == 3 && fields[2] == name {
            ensure!(found.is_none(), "多个窗口标记为 {name}，请手动检查");
            found = Some(fields[0].to_owned());
        } else if fields.get(1) == Some(&name) {
            collision = true;
        }
    }
    ensure!(
        found.is_some() || !collision,
        "存在同名非 twig 窗口：{name}，请先重命名"
    );
    Ok(found)
}

pub fn ensure_window(
    config: &Config,
    project: &Project,
    name: &str,
    path: &Path,
) -> Result<String> {
    ensure_session(config, project)?;
    if let Some(id) = window(config, project, name)? {
        return Ok(id);
    }
    let id = tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        &format!("{}:", config.session),
        "-n",
        name,
        "-c",
        path.to_str().unwrap(),
    ])?;
    tmux(&[
        "set-option",
        "-w",
        "-t",
        &id,
        "automatic-rename",
        "off",
        ";",
        "set-option",
        "-w",
        "-t",
        &id,
        "@twig-feature",
        name,
    ])?;
    window(config, project, name)?
        .ok_or_else(|| anyhow::anyhow!("窗口创建后未找到，请重试 twig open {name}"))
}

// 窗口中只有空闲 Shell 才允许 rm；包含后台任务、exec 后的程序或未知状态均阻止。
pub fn idle(window: &str) -> Result<bool> {
    let panes = tmux(&[
        "list-panes",
        "-t",
        window,
        "-F",
        "#{pane_pid}\t#{pane_current_command}\t#{pane_dead}",
    ])?;
    let processes = checked("ps", &["-axo".as_ref(), "ppid=".as_ref()], None)?;
    for pane in panes.lines() {
        let fields: Vec<_> = pane.split('\t').collect();
        ensure!(fields.len() == 3, "无法解析 Pane 状态");
        if fields[2] == "1" {
            continue;
        }
        if !matches!(
            fields[1].trim_start_matches('-'),
            "bash" | "zsh" | "sh" | "fish" | "dash" | "ksh"
        ) {
            return Ok(false);
        }
        if processes.lines().any(|parent| parent.trim() == fields[0]) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn enter(config: &Config, window: &str) -> Result<()> {
    tmux(&["select-window", "-t", window])?;
    let action = if std::env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = Command::new("tmux")
        .args([action, "-t", &format!("={}", config.session)])
        .status()?;
    ensure!(
        status.success(),
        "tmux 进入失败；环境已保留，可在交互式终端执行 twig open"
    );
    Ok(())
}
