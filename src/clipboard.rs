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
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::{
    io::{self, Write},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Notice {
    message: String,
    created: Instant,
    pending: bool,
    generation: u64,
}

impl Notice {
    fn new(message: String) -> Self {
        Self {
            message,
            created: Instant::now(),
            pending: false,
            generation: COPIER
                .get()
                .map_or(0, |copier| copier.latest.load(Ordering::Relaxed)),
        }
    }

    pub fn expired(&self) -> bool {
        COPIER.get().is_some_and(|copier| {
            self.superseded(
                copier.latest.load(Ordering::Relaxed),
                copier.finished.load(Ordering::Relaxed),
            )
        }) || (!self.pending && self.created.elapsed() >= Duration::from_secs(3))
    }

    fn superseded(&self, latest: u64, finished: u64) -> bool {
        latest > self.generation || (self.pending && finished >= self.generation)
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

struct Delivery {
    message: String,
    terminal: Option<String>,
}
type CopyResult = (u64, Result<Delivery>, mpsc::Sender<()>);

fn copy_worker(
    input: mpsc::Receiver<(u64, String)>,
    output: mpsc::Sender<CopyResult>,
    mut prepare: impl FnMut(&str) -> Result<Delivery>,
) {
    for (id, text) in input {
        let (ack, delivered) = mpsc::channel();
        if output.send((id, prepare(&text), ack)).is_err() || delivered.recv().is_err() {
            break;
        }
    }
}

struct Copier {
    requests: mpsc::Sender<(u64, String)>,
    results: Mutex<mpsc::Receiver<CopyResult>>,
    latest: AtomicU64,
    finished: AtomicU64,
}
static COPIER: OnceLock<Copier> = OnceLock::new();

// 单一工作线程按顺序写系统剪贴板；OSC 52 只由当前 UI 线程输出。
pub fn start(text: String) -> Notice {
    let copier = COPIER.get_or_init(|| {
        let (requests, input) = mpsc::channel::<(u64, String)>();
        let (output, results) = mpsc::channel();
        std::thread::spawn(move || copy_worker(input, output, prepare));
        Copier {
            requests,
            results: Mutex::new(results),
            latest: AtomicU64::new(0),
            finished: AtomicU64::new(0),
        }
    });
    let id = copier.latest.fetch_add(1, Ordering::Relaxed) + 1;
    let mut notice = Notice::new(format!("正在复制 {} chars", text.chars().count()));
    notice.pending = true;
    if copier.requests.send((id, text)).is_err() {
        return Notice::new("复制失败：剪贴板工作线程已停止".into());
    }
    notice
}

pub fn poll() -> Option<Notice> {
    let copier = COPIER.get()?;
    let mut notice = None;
    for (id, delivery, ack) in copier.results.lock().unwrap().try_iter() {
        let result = delivery.and_then(|delivery| {
            if let Some(sequence) = delivery.terminal {
                let mut output = io::stdout().lock();
                output.write_all(sequence.as_bytes())?;
                output.flush()?;
            }
            Ok(delivery.message)
        });
        copier.finished.store(id, Ordering::Relaxed);
        let _ = ack.send(());
        if id == copier.latest.load(Ordering::Relaxed) {
            notice = Some(Notice::new(
                result.unwrap_or_else(|e| format!("复制失败：{e}")),
            ));
        }
    }
    notice
}

fn prepare(text: &str) -> Result<Delivery> {
    let chars = text.chars().count();
    let remote =
        std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
    let native_error = if remote {
        None
    } else {
        match native_copy(text) {
            Ok(()) => {
                return Ok(Delivery {
                    message: format!("已复制 {chars} chars"),
                    terminal: None,
                });
            }
            Err(error) => Some(error),
        }
    };
    // OSC 52 没有成功回执，不能声称系统剪贴板已更新。
    ensure!(text.len() <= 100_000, "选区超过终端剪贴板的 100 KB 限制");
    let terminal = if std::env::var_os("TMUX").is_some() {
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
        None
    } else {
        Some(osc52(text))
    };
    Ok(Delivery {
        terminal,
        message: match native_error {
            Some(error) => format!("已请求复制 {chars} chars；本机剪贴板不可用（{error}）"),
            None => format!("已请求复制 {chars} chars"),
        },
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
    fn returning_to_a_view_does_not_restore_a_finished_pending_notice() {
        let mut notice = Notice {
            message: "正在复制".into(),
            created: Instant::now(),
            pending: true,
            generation: 1,
        };
        assert!(!notice.superseded(1, 0));
        assert!(notice.superseded(1, 1));
        notice.pending = false;
        assert!(!notice.superseded(1, 1));
        assert!(notice.superseded(2, 1));
    }
    #[test]
    fn copy_worker_waits_for_terminal_delivery_before_the_next_copy() {
        let (requests, input) = mpsc::channel();
        let (output, results) = mpsc::channel();
        let (started, observed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            copy_worker(input, output, |text| {
                started.send(text.to_owned()).unwrap();
                Ok(Delivery {
                    message: text.into(),
                    terminal: Some(osc52(text)),
                })
            })
        });
        requests.send((1, "first".into())).unwrap();
        requests.send((2, "second".into())).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            "first"
        );
        let (id, first, ack) = results.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(id, 1);
        assert_eq!(first.unwrap().terminal, Some(osc52("first")));
        assert!(observed.recv_timeout(Duration::from_millis(30)).is_err());
        ack.send(()).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            "second"
        );
        let (id, _, ack) = results.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(id, 2);
        ack.send(()).unwrap();
        drop(requests);
        worker.join().unwrap();
    }
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
