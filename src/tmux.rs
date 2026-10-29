use crate::{
    config::{Config, Project},
    git::{checked, run},
};
use anyhow::{Context, Result, bail, ensure};
use std::{ffi::OsStr, path::Path};

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
            ";",
            "set-option",
            "-w",
            "-t",
            &format!("{}:main", config.session),
            "@twig-feature",
            "main",
        ])?;
    }
    Ok(())
}

pub fn window(config: &Config, project: &Project, name: &str) -> Result<Option<String>> {
    window_in(&windows(config, project)?, name)
}

pub fn windows(config: &Config, project: &Project) -> Result<String> {
    if !session_exists(config, project)? {
        return Ok(String::new());
    }
    tmux(&[
        "list-windows",
        "-t",
        &format!("={}", config.session),
        "-F",
        "#{window_id}\t#{window_name}\t#{@twig-feature}",
    ])
}

pub fn window_in(output: &str, name: &str) -> Result<Option<String>> {
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
        ensure!(
            tmux(&["show-options", "-wqv", "-t", &id, "@twig-startup-pending"])? != "1",
            "窗口初始化未完成，已保留 Pane；请检查后手动修复，或 twig stop 后重新打开"
        );
        return Ok(id);
    }
    let layout = if name == "main" {
        crate::startup::Layout::default()
    } else if let Some(thread) = project
        .state()?
        .get(name)
        .and_then(|task| task.agent_thread.clone())
    {
        crate::agent::task_layout(project, &thread)?
    } else {
        crate::startup::load(config, project)?
    };
    let target = format!("{}:", config.session);
    let new_target = format!("{}:={name}", config.session);
    let mut args = vec![
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        &target,
        "-n",
        name,
        "-c",
        path.to_str().unwrap(),
    ];
    if !layout.panes.is_empty() {
        args.extend([
            "",
            ";",
            "set-option",
            "-w",
            "-t",
            &new_target,
            "remain-on-exit",
            "on",
        ]);
    }
    let id = tmux(&args)?;
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
    if !layout.panes.is_empty() {
        tmux(&["set-option", "-w", "-t", &id, "@twig-startup-pending", "1"])?;
        layout
            .apply(&id, path.to_str().unwrap())
            .context("任务窗口初始化失败，已保留窗口与输出；检查后可 stop 再 open")?;
        tmux(&["set-option", "-wu", "-t", &id, "@twig-startup-pending"])?;
    }
    window(config, project, name)?
        .ok_or_else(|| anyhow::anyhow!("窗口创建后未找到，请重试 twig open <项目名> {name}"))
}

// 初版 init 的主窗口没有 @twig-feature；仅在已验证的 Session 内，
// 同时核对 main 名称和启动目录，才能复用该历史主窗口。
pub fn ensure_main_window(config: &Config, project: &Project) -> Result<String> {
    ensure_session(config, project)?;
    let output = tmux(&[
        "list-windows",
        "-t",
        &format!("={}", config.session),
        "-F",
        "#{window_id}\t#{window_name}\t#{@twig-feature}\t#{pane_start_path}",
    ])?;
    let mut found = None;
    let mut collision = false;
    for line in output.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        ensure!(fields.len() == 4, "无法解析主窗口信息");
        if fields[2] == "main"
            || (fields[1] == "main"
                && fields[2].is_empty()
                && Path::new(fields[3]).canonicalize().ok().as_deref()
                    == Some(config.main.as_path()))
        {
            ensure!(found.is_none(), "多个窗口标记为 main，请手动检查");
            found = Some(fields[0].to_owned());
        } else if fields[1] == "main" {
            collision = true;
        }
    }
    if let Some(id) = found {
        return Ok(id);
    }
    ensure!(!collision, "存在无法确认归属的 main 窗口，请先重命名");
    ensure_window(config, project, "main", &config.main)
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
    ensure!(!panes.is_empty(), "无法确认空窗口的进程状态");
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
        let pid = fields[0].parse().context("无法解析 Pane PID")?;
        if has_children(pid)? {
            return Ok(false);
        }
    }
    Ok(true)
}

// 只查询目标 Pane 的直接子进程。失败不视作空闲，不回退到全机扫描。
fn has_children(pid: libc::pid_t) -> Result<bool> {
    ensure!(pid > 0, "无效的 Pane PID：{pid}");
    children(pid).with_context(|| format!("无法确认 Pane 进程 {pid} 的子进程"))
}

#[cfg(target_os = "linux")]
fn children(pid: libc::pid_t) -> Result<bool> {
    // 子进程归属创建它的线程；必须检查线程组中的所有线程。
    linux_children(Path::new(&format!("/proc/{pid}/task")))
}
#[cfg(target_os = "linux")]
fn linux_children(tasks: &Path) -> Result<bool> {
    let mut found = false;
    for task in std::fs::read_dir(tasks)? {
        let task = task?;
        found = true;
        let children = std::fs::read_to_string(task.path().join("children"))?;
        if !children.trim().is_empty() {
            return Ok(true);
        }
    }
    ensure!(found, "进程线程列表为空");
    Ok(false)
}

#[cfg(target_os = "macos")]
fn children(pid: libc::pid_t) -> Result<bool> {
    // libproc 将部分错误转为 0，必须同时检查 errno，不能把错误视为无子进程。
    // 仅需判断是否存在子进程，一个 PID 的缓冲区即可。
    let mut child: libc::pid_t = 0;
    unsafe {
        if libc::kill(pid, 0) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        *libc::__error() = 0;
        let count = libc::proc_listchildpids(
            pid,
            (&raw mut child).cast(),
            std::mem::size_of_val(&child) as libc::c_int,
        );
        if count < 0 || (count == 0 && *libc::__error() != 0) {
            return Err(std::io::Error::last_os_error().into());
        }
        if libc::kill(pid, 0) != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(count > 0)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn children(_pid: libc::pid_t) -> Result<bool> {
    bail!("当前平台不支持安全的 Pane 子进程查询")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    #[test]
    fn targeted_children_detect_jobs_and_reject_disappeared_processes() {
        let output = tempfile::NamedTempFile::new().unwrap();
        let mut shell = Command::new("sh").args(["-c",
            "echo idle; read line; sleep 30 & child=$!; echo busy; read line; kill $child; wait $child; echo done; read line"])
            .stdin(Stdio::piped()).stdout(output.reopen().unwrap()).stderr(Stdio::null()).spawn().unwrap();
        let pid = shell.id() as libc::pid_t;
        let wait = |word: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !std::fs::read_to_string(output.path())
                .unwrap()
                .contains(word)
            {
                assert!(
                    Instant::now() < deadline,
                    "等待 Shell {pid} 输出 {word} 超时"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait("idle");
        assert!(!has_children(pid).unwrap());
        shell.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
        wait("busy");
        assert!(has_children(pid).unwrap());
        shell.stdin.as_mut().unwrap().write_all(b"stop\n").unwrap();
        wait("done");
        assert!(!has_children(pid).unwrap());
        shell.kill().unwrap();
        shell.wait().unwrap();
        assert!(has_children(pid).is_err());
        assert!(has_children(0).is_err());
        assert!(has_children(-1).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_checks_all_threads_and_rejects_incomplete_reads() {
        let root = tempfile::tempdir().unwrap();
        assert!(linux_children(root.path()).is_err());
        for tid in ["leader", "worker"] {
            std::fs::create_dir(root.path().join(tid)).unwrap();
            std::fs::write(root.path().join(tid).join("children"), "").unwrap();
        }
        assert!(!linux_children(root.path()).unwrap());
        std::fs::write(root.path().join("worker/children"), "123 ").unwrap();
        assert!(linux_children(root.path()).unwrap());
        std::fs::remove_file(root.path().join("worker/children")).unwrap();
        assert!(linux_children(root.path()).is_err());
    }
}
