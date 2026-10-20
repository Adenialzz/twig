use std::io::{self, Write};

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Topic {
    /// 安装、依赖和更新
    Installation,
    /// 从登记项目到完成任务
    Quickstart,
    /// 全部命令的参数、默认值与副作用
    Commands,
    /// 登记表、配置优先级和状态文件
    Configuration,
    /// 分屏、启动命令和 shell/process 模式
    Windows,
    /// 控制台操作、快捷键与会话状态
    Console,
    /// 停止窗口、安全删除与文件保护
    Cleanup,
    /// 推送及创建草稿 PR/MR
    Pr,
    /// Zsh 补全安装与使用
    Completion,
    /// Agent 和脚本的非交互调用
    Automation,
    /// 错误诊断和资源恢复
    Troubleshooting,
}

impl Topic {
    fn content(self) -> &'static str {
        match self {
            Self::Installation => include_str!("../docs/installation.md"),
            Self::Quickstart => include_str!("../docs/quickstart.md"),
            Self::Commands => include_str!("../docs/commands.md"),
            Self::Configuration => include_str!("../docs/configuration.md"),
            Self::Windows => include_str!("../docs/windows.md"),
            Self::Console => include_str!("../docs/console.md"),
            Self::Cleanup => include_str!("../docs/cleanup.md"),
            Self::Pr => include_str!("../docs/pr.md"),
            Self::Completion => include_str!("../docs/completion.md"),
            Self::Automation => include_str!("../docs/automation.md"),
            Self::Troubleshooting => include_str!("../docs/troubleshooting.md"),
        }
    }
}

pub fn print(topic: Option<Topic>, all: bool) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(
        topic
            .map_or(include_str!("../docs/index.md"), Topic::content)
            .as_bytes(),
    )?;
    if all {
        for topic in Topic::value_variants() {
            out.write_all(b"\n---\n\n")?;
            out.write_all(topic.content().as_bytes())?;
        }
    }
    Ok(())
}
