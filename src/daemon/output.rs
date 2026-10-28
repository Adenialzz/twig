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
