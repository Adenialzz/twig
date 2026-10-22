//! Console 采集线程的同步命令限制；普通 CLI 不改变执行语义。
use anyhow::{Context, Result, bail};
use std::{
    cell::RefCell,
    io::{Read, Seek},
    os::unix::process::CommandExt,
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

thread_local! { static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) }; }

pub fn install(cancel: Arc<AtomicBool>) {
    CANCEL.with(|slot| *slot.borrow_mut() = Some(cancel));
}
pub fn cancelled() -> bool {
    CANCEL.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    })
}
pub fn check() -> Result<()> {
    if cancelled() {
        bail!("查询已取消");
    }
    Ok(())
}

pub fn output(command: &mut Command) -> Result<Output> {
    let enabled = CANCEL.with(|slot| slot.borrow().is_some());
    if !enabled {
        return Ok(command.output()?);
    }
    check()?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    // 独立进程组使超时能回收 CLI 包装器及其子进程；文件避免输出管道塞满。
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    let mut child = command.spawn().context("无法启动查询命令")?;
    let start = Instant::now();
    let mut delay = 1;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancelled() || start.elapsed() >= Duration::from_secs(2) {
            // 此 PID 是本次新建进程组的组长，绝不操作用户已有进程组。
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!("查询取消或超过 2 秒：{:?}", command.get_program());
        }
        std::thread::sleep(Duration::from_millis(delay));
        delay = (delay * 2).min(10);
    };
    stdout.rewind()?;
    stderr.rewind()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out)?;
    stderr.read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_reaps_query_and_preserves_interactive_thread() {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            install(worker_cancel);
            output(Command::new("sh").args(["-c", "sleep 30"]))
        });
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        assert!(worker.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!cancelled());
    }
}
