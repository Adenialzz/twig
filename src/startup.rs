use crate::{
    config::{Config, Project},
    registry,
    tmux::tmux,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(default)]
    pub panes: Vec<Pane>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pane {
    #[serde(default)]
    pub mode: Mode,
    pub command: Option<Vec<String>>,
    pub split: Option<Split>,
    pub percent: Option<u8>,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Shell,
    Process,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Right,
    Down,
}

pub fn load(config: &Config, project: &Project) -> Result<Layout> {
    #[derive(Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ProjectConfig {
        tmux: Option<Layout>,
    }
    let path = config.main.join(".twig/config.toml");
    let local: ProjectConfig = match fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("配置格式错误：{}", path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ProjectConfig::default(),
        Err(e) => return Err(e).with_context(|| format!("无法读取 {}", path.display())),
    };
    let layout = match local.tmux {
        Some(layout) => layout,
        None => registry::load(&project.registry)?.tmux.unwrap_or_default(),
    };
    layout.validate().context("tmux 启动配置错误")?;
    Ok(layout)
}

impl Layout {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.panes.iter().filter(|p| p.focus).count() <= 1,
            "只能指定一个 focus Pane"
        );
        for (index, pane) in self.panes.iter().enumerate() {
            ensure!(
                pane.mode != Mode::Process || pane.command.is_some(),
                "process 模式必须提供 command"
            );
            ensure!(
                if index == 0 {
                    pane.split.is_none() && pane.percent.is_none()
                } else {
                    pane.split.is_some()
                },
                "首个 Pane 不设置 split/percent，后续 Pane 必须设置 split"
            );
            if let Some(percent) = pane.percent {
                ensure!((1..100).contains(&percent), "percent 必须在 1–99 之间");
            }
            if let Some(command) = &pane.command {
                ensure!(
                    command.first().is_some_and(|s| !s.is_empty()),
                    "command 必须包含非空程序名"
                );
                ensure!(
                    command.iter().all(|s| !s.contains('\0')),
                    "command 不能包含 NUL"
                );
            }
        }
        Ok(())
    }

    /// 先建好全部 Pane，再启动 Shell 或进程，布局失败时不执行启动命令。
    pub fn apply(&self, window: &str, path: &str) -> Result<()> {
        tmux(&["set-option", "-w", "-t", window, "remain-on-exit", "on"])?;
        let first = tmux(&["display-message", "-p", "-t", window, "#{pane_id}"])?;
        let mut ids = vec![first];
        for pane in self.panes.iter().skip(1) {
            let direction = match pane.split {
                Some(Split::Right) => "-h",
                _ => "-v",
            };
            let size = format!("{}%", pane.percent.unwrap_or(50));
            ids.push(tmux(&[
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                ids.last().unwrap(),
                direction,
                "-l",
                &size,
                "-c",
                path,
                "",
            ])?);
        }
        let shell = tmux(&["show-options", "-Av", "-t", window, "default-shell"])?;
        for (pane, id) in self.panes.iter().zip(&ids) {
            let shell_command = vec![shell.clone(), "-l".into()];
            let command = match pane.mode {
                Mode::Shell => &shell_command,
                Mode::Process => pane.command.as_ref().unwrap(),
            };
            // tmux 将参数末尾的分号解释为命令分隔符，需转义才能原样传给程序。
            let command: Vec<String> = command
                .iter()
                .map(|arg| match arg.strip_suffix(';') {
                    Some(prefix) => format!("{prefix}\\;"),
                    None => arg.clone(),
                })
                .collect();
            if pane.mode == Mode::Process {
                tmux(&["set-option", "-p", "-t", id, "remain-on-exit", "on"])?;
            }
            let mut args = vec![
                "respawn-pane",
                "-k",
                "-t",
                id,
                "-c",
                path,
                "/usr/bin/env",
                "--",
            ];
            args.extend(command.iter().map(String::as_str));
            tmux(&args)?;
            if pane.mode == Mode::Shell {
                tmux(&["set-option", "-p", "-t", id, "remain-on-exit", "off"])?;
            }
        }
        // 仅 process Pane 显式保留退出输出；后续手动分屏沿用普通关闭行为。
        tmux(&["set-option", "-w", "-t", window, "remain-on-exit", "off"])?;
        for (pane, id) in self.panes.iter().zip(&ids) {
            if pane.mode == Mode::Process {
                continue;
            }
            if let Some(command) = &pane.command {
                // command 是参数数组；引用后作为 Shell 的第一条输入，不展开参数内容。
                let line = format!(
                    " /usr/bin/env -- {}",
                    command
                        .iter()
                        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                tmux(&["send-keys", "-t", id, "-l", &line])?;
                tmux(&["send-keys", "-t", id, "Enter"])?;
            }
        }
        let focus = self.panes.iter().position(|p| p.focus).unwrap_or(0);
        tmux(&["select-pane", "-t", &ids[focus]])?;
        Ok(())
    }
}
