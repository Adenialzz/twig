use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::Paragraph,
};
use ratatui::{
    text::Line,
    widgets::{Block, Borders, Clear, Wrap},
};
use std::sync::Mutex;
use std::{
    io::{self, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Notice {
    message: String,
    created: Instant,
}

impl Notice {
    pub fn new(message: String) -> Self {
        Self {
            message,
            created: Instant::now(),
        }
    }

    pub fn expired(&self) -> bool {
        self.created.elapsed() >= Duration::from_secs(3)
    }

    pub fn draw(&self, frame: &mut ratatui::Frame) {
        let screen = frame.area();
        let width = (Line::from(self.message.as_str()).width() + 4)
            .min(72)
            .min(usize::from(screen.width)) as u16;
        let inner_width = usize::from(width.saturating_sub(2)).max(1);
        let height = (Line::from(self.message.as_str())
            .width()
            .div_ceil(inner_width)
            + 2)
        .min(6)
        .min(usize::from(screen.height.saturating_sub(1))) as u16;
        let area = Rect::new(
            screen.right() - width,
            screen.bottom().saturating_sub(height + 1),
            width,
            height,
        );
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(self.message.as_str())
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::White).bg(Color::Indexed(24)))
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
    }
}

// X11/Wayland 的剪贴板所有者必须存活到进程结束，返回导航时也不能释放。
static NATIVE: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub fn copy(text: &str) -> Result<String> {
    let chars = text.chars().count();
    let remote =
        std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
    let native_error = if remote {
        None
    } else {
        match native_copy(text) {
            Ok(()) => return Ok(format!("已复制 {chars} chars")),
            Err(error) => Some(error),
        }
    };
    // OSC 52 没有成功回执，不能声称系统剪贴板已更新。
    ensure!(text.len() <= 100_000, "选区超过终端剪贴板的 100 KB 限制");
    if std::env::var_os("TMUX").is_some() {
        // 使用外层 tmux 的剪贴板通道，不向被捕获的任务 Pane 写转义序列。
        let mut child = Command::new("tmux")
            .args(["load-buffer", "-w", "-b", "twig-selection", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let written = child
            .stdin
            .take()
            .context("缺少剪贴板 stdin")?
            .write_all(text.as_bytes());
        let output = child.wait_with_output()?;
        written?;
        ensure!(
            output.status.success(),
            "外层 tmux 复制失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        let mut output = io::stdout().lock();
        output.write_all(osc52(text).as_bytes())?;
        output.flush()?;
    }
    Ok(match native_error {
        Some(error) => format!("已请求复制 {chars} chars；本机剪贴板不可用（{error}）"),
        None => format!("已请求复制 {chars} chars"),
    })
}

fn native_copy(text: &str) -> Result<()> {
    let mut owner = NATIVE
        .lock()
        .map_err(|_| anyhow::anyhow!("剪贴板锁已损坏"))?;
    if owner.is_none() {
        *owner = Some(arboard::Clipboard::new()?);
    }
    owner.as_mut().unwrap().set_text(text)?;
    Ok(())
}

fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_copy_encodes_text_without_control_injection() {
        let text = "中文\n\x1b]52;c;injected\x07";
        let sequence = osc52(text);
        let encoded = sequence
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(STANDARD.decode(encoded).unwrap(), text.as_bytes());
        assert!(!encoded.contains('\x1b'));
    }
}
