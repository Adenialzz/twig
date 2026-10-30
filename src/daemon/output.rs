//! 业务输出与终端连接结果；后台不直接写客户端终端。
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, path::PathBuf};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub terminal: Option<(PathBuf, crate::console::TerminalTarget)>,
}
thread_local! { static CURRENT: RefCell<Option<Output>> = const { RefCell::new(None) }; }

pub fn line(text: String, error: bool) {
    CURRENT.with(|current| {
        if let Some(output) = current.borrow_mut().as_mut() {
            let buffer = if error {
                &mut output.stderr
            } else {
                &mut output.stdout
            };
            buffer.push_str(&text);
            buffer.push('\n');
        } else if error {
            eprintln!("{text}");
        } else {
            println!("{text}");
        }
    });
}
pub fn terminal(path: PathBuf, target: crate::console::TerminalTarget) {
    CURRENT.with(|current| {
        current
            .borrow_mut()
            .as_mut()
            .expect("后台操作上下文")
            .terminal = Some((path, target))
    });
}
pub fn capture(action: impl FnOnce() -> anyhow::Result<()>) -> (Output, anyhow::Result<()>) {
    CURRENT.with(|current| *current.borrow_mut() = Some(Output::default()));
    let result = action();
    let output = CURRENT.with(|current| current.borrow_mut().take().unwrap());
    (output, result)
}
#[macro_export]
macro_rules! output { ($($arg:tt)*) => { $crate::daemon::output::line(format!($($arg)*), false) }; }
#[macro_export]
macro_rules! diagnostic { ($($arg:tt)*) => { $crate::daemon::output::line(format!($($arg)*), true) }; }

/// 不丢弃执行或预检查失败前已产生的诊断。
pub fn checked(output: Output, result: anyhow::Result<()>) -> anyhow::Result<Output> {
    use anyhow::Context;
    if output.stdout.is_empty() && output.stderr.is_empty() {
        result?;
    } else {
        result.with_context(|| format!("{}{}", output.stdout, output.stderr))?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    #[test]
    fn failure_keeps_captured_diagnostics_and_original_cause() {
        let (output, result) = super::capture(|| {
            crate::output!("执行信息");
            crate::diagnostic!("受保护文件：keep.txt");
            anyhow::bail!("拒绝删除")
        });
        let error = super::checked(output, result).err().unwrap();
        let text = format!("{error:#}");
        for expected in ["执行信息", "keep.txt", "拒绝删除"] {
            assert!(text.contains(expected), "{text}");
        }
    }
}
