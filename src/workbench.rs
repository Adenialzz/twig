//! Console 的 tmux Control Mode 终端视图；tmux 保存唯一的终端运行状态。
use crate::clipboard;
use crate::notification::{PickerAction, UnreadPicker, UnreadTask};
mod selection;
use alacritty_terminal::{
    Term,
    event::VoidListener,
    grid::Dimensions,
    index::{Column, Line, Point},
    term::{
        Config,
        cell::{Cell, Flags},
    },
    vte::ansi::{Color as AnsiColor, Processor},
};
use anyhow::{Context, Result, bail, ensure};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};
use selection::Selection;
use std::{
    collections::{HashSet, VecDeque},
    io::{self, IsTerminal, Read, Write},
    os::fd::AsRawFd,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Args {
    socket: Option<PathBuf>,
    session: String,
}
impl Args {
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.arg("-S").arg(socket);
        }
        command
    }
}

struct Client {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    bytes: Vec<u8>,
    lines: VecDeque<Vec<u8>>,
    events: VecDeque<Vec<u8>>,
    closed: bool,
}

impl Client {
    fn connect(args: &Args) -> Result<Self> {
        // has-session 不启动 server，也不隐式创建会话。
        let status = args
            .command()
            .args(["has-session", "-t", &format!("={}", args.session)])
            .output()?;
        ensure!(
            status.status.success(),
            "无法连接指定 tmux 会话：{}",
            String::from_utf8_lossy(&status.stderr).trim()
        );
        let mut child = args
            .command()
            .args(["-C", "attach-session", "-t", &format!("={}", args.session)])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().context("缺少 control stdin")?;
        let output = child.stdout.take().context("缺少 control stdout")?;
        let mut client = Self {
            child,
            input,
            output,
            bytes: Vec::new(),
            lines: VecDeque::new(),
            events: VecDeque::new(),
            closed: false,
        };
        let fd = client.output.as_raw_fd();
        // SAFETY: fd 属于存活的 ChildStdout；只修改该管道的读端。
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(flags != -1, "读取管道标志失败");
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != -1,
            "设置非阻塞管道失败"
        );
        client.response()?;
        Ok(client)
    }

    fn pump(&mut self) -> Result<()> {
        // 限制单次读取，避免大量输出饿死 UI。积压过大时显式失败并保留 server。
        let mut buf = [0; 8192];
        for _ in 0..32 {
            match self.output.read(&mut buf) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => self.bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        ensure!(self.bytes.len() < 4 * 1024 * 1024, "control 单行输出过大");
        let mut consumed = 0;
        for (end, byte) in self.bytes.iter().enumerate() {
            if *byte == b'\n' {
                self.lines.push_back(self.bytes[consumed..end].to_vec());
                consumed = end + 1;
            }
        }
        self.bytes.drain(..consumed);
        Ok(())
    }

    fn response(&mut self) -> Result<Vec<Vec<u8>>> {
        let until = Instant::now() + Duration::from_secs(3);
        let mut block: Option<Vec<u8>> = None;
        let mut result = Vec::new();
        loop {
            while let Some(line) = self.lines.pop_front() {
                if let Some(id) = &block {
                    if line.strip_prefix(b"%end ") == Some(id.as_slice()) {
                        return Ok(result);
                    }
                    if line.strip_prefix(b"%error ") == Some(id.as_slice()) {
                        bail!(
                            "tmux 命令失败：{}",
                            String::from_utf8_lossy(&result.concat())
                        );
                    }
                    result.push(line);
                } else if let Some(id) = line.strip_prefix(b"%begin ") {
                    block = Some(id.to_vec());
                } else if line.starts_with(b"%exit") {
                    bail!(
                        "tmux control 客户端退出：{}",
                        String::from_utf8_lossy(&line)
                    );
                } else {
                    self.events.push_back(line);
                    ensure!(
                        self.events.len() < 16384,
                        "control 事件积压过多，终端停止读取"
                    );
                }
            }
            ensure!(!self.closed, "tmux control 连接已关闭，后台资源未删除");
            ensure!(Instant::now() < until, "等待 tmux 应答超时");
            self.pump()?;
            if self.lines.is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn command(&mut self, command: &str) -> Result<Vec<Vec<u8>>> {
        Ok(self.batch(&[command.to_owned()])?.remove(0))
    }

    /// 只批量执行不等待外部事件的 tmux 内建命令。
    /// tmux 在一次 cmdq_next 中执行整组命令，不在快照组成部分间读取 PTY。
    fn batch(&mut self, commands: &[String]) -> Result<Vec<Vec<Vec<u8>>>> {
        writeln!(self.input, "{}", commands.join(" ; "))?;
        self.input.flush()?;
        commands.iter().map(|_| self.response()).collect()
    }

    fn key(&mut self, pane: &str, key: event::KeyEvent) -> Result<()> {
        if let Some(name) = key_name(key) {
            self.command(&format!("send-keys -t {pane} {name}"))?;
        } else if let Some(bytes) = key_bytes(key) {
            self.send(pane, &bytes)?;
        }
        Ok(())
    }

    fn paste(&mut self, pane: &str, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        ensure!(!text.contains('\0'), "粘贴文本不能包含 NUL");
        let name = format!("twig-input-{}", std::process::id());
        // 每块限长，八进制编码确保引号、换行、$ 和分号不成为命令语法。
        let result = (|| {
            for (index, chunk) in text.as_bytes().chunks(4096).enumerate() {
                let value = chunk
                    .iter()
                    .map(|b| format!("\\{b:03o}"))
                    .collect::<String>();
                self.command(&format!(
                    "set-buffer {} -b {name} \"{value}\"",
                    if index == 0 { "" } else { "-a" }
                ))?;
            }
            // -p 按 server 此刻的模式加标记；-r 保留多行文本中的换行。
            self.command(&format!("paste-buffer -p -r -d -b {name} -t {pane}"))
        })();
        if result.is_err() {
            let _ = self.command(&format!("delete-buffer -b {name}"));
        }
        result?;
        Ok(())
    }

    fn send(&mut self, pane: &str, text: &[u8]) -> Result<()> {
        // 字节编码避免输入成为 tmux 命令语法；分块限制命令行长度。
        for chunk in text.chunks(256) {
            let hex = chunk
                .iter()
                .map(|b| format!(" {b:02x}"))
                .collect::<String>();
            self.command(&format!("send-keys -t {pane} -H{hex}"))?;
        }
        Ok(())
    }

    fn wheel(&mut self, pane: &Pane, mouse: event::MouseEvent) -> Result<bool> {
        // 模式以 server 此刻的状态为准，不从字符快照推断或缓存。
        let state = self.command(&format!(
            "display-message -p -t {} '#{{mouse_any_flag}} #{{mouse_sgr_flag}} #{{mouse_utf8_flag}}'",
            pane.id
        ))?;
        let state = String::from_utf8(state.concat())?;
        let flags = state.split_whitespace().collect::<Vec<_>>();
        ensure!(flags.len() == 3, "无法读取鼠标模式：{state}");
        if flags[0] == "0" {
            return Ok(false);
        }
        let bytes = wheel_bytes(mouse, pane.rect, flags[1] == "1", flags[2] == "1");
        self.send(&pane.id, &bytes)?;
        Ok(true)
    }
}

fn wheel_bytes(mouse: event::MouseEvent, rect: Rect, sgr: bool, utf8: bool) -> Vec<u8> {
    let mut button = if mouse.kind == MouseEventKind::ScrollUp {
        64
    } else {
        65
    };
    for (modifier, bit) in [
        (KeyModifiers::SHIFT, 4),
        (KeyModifiers::ALT, 8),
        (KeyModifiers::CONTROL, 16),
    ] {
        if mouse.modifiers.contains(modifier) {
            button |= bit;
        }
    }
    let x = u32::from(mouse.column - rect.x) + 1;
    let y = u32::from(mouse.row - rect.y) + 1;
    if sgr {
        return format!("\x1b[<{button};{x};{y}M").into_bytes();
    }
    let mut bytes = b"\x1b[M".to_vec();
    // 与 tmux 一致，1005 使用双字节 UTF-8；传统协议坐标限制为单字节。
    if utf8 {
        if x + 32 > 0x7ff || y + 32 > 0x7ff {
            return Vec::new();
        }
        for value in [button + 32, x + 32, y + 32] {
            bytes.extend_from_slice(
                char::from_u32(value)
                    .unwrap()
                    .encode_utf8(&mut [0; 4])
                    .as_bytes(),
            );
        }
    } else {
        bytes.extend([
            button as u8 + 32,
            (x + 32).min(255) as u8,
            (y + 32).min(255) as u8,
        ]);
    }
    bytes
}

impl Drop for Client {
    fn drop(&mut self) {
        // 只结束本客户端，绝不 kill-session/server 或 Pane。
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn output_pane(line: &[u8]) -> Option<&str> {
    let rest = line.strip_prefix(b"%output ")?;
    let end = rest.iter().position(|&b| b == b' ')?;
    std::str::from_utf8(&rest[..end]).ok()
}

struct Pane {
    id: String,
    rect: Rect,
    active: bool,
    parser: TerminalState,
    cursor: Option<(u16, u16)>,
    cursor_style: crossterm::cursor::SetCursorStyle,
    scroll_offset: usize,
    title: String,
}

struct TerminalSize {
    rows: usize,
    cols: usize,
}
impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct TerminalState {
    term: Term<VoidListener>,
    processor: Processor,
}
impl TerminalState {
    fn new(rows: u16, cols: u16, history: usize) -> Self {
        let size = TerminalSize {
            rows: rows.into(),
            cols: cols.into(),
        };
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &size, VoidListener),
            processor: Processor::new(),
        }
    }
    fn process(&mut self, bytes: &[u8]) {
        // 协议应答由 tmux 负责；此处只解析屏幕快照，不回传终端查询或剪贴板事件。
        self.processor.advance(&mut self.term, bytes);
    }
    fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.term.grid()[Point::new(Line(i32::from(row)), Column(col.into()))]
    }
    #[cfg(test)]
    fn text(&self) -> String {
        (0..self.term.screen_lines())
            .map(|row| {
                (0..self.term.columns())
                    .filter_map(|col| {
                        let cell = self.cell(row as u16, col as u16);
                        (!cell.flags.contains(Flags::WIDE_CHAR_SPACER)).then_some(cell.c)
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end_matches('\n')
            .to_owned()
    }
}

fn reload(client: &mut Client) -> Result<Vec<Pane>> {
    let lines = client.command("list-panes -F '#{pane_id}'")?;
    let mut panes = Vec::new();
    for line in lines {
        let id = String::from_utf8(line)?;
        ensure!(
            id.starts_with('%') && id[1..].bytes().all(|b| b.is_ascii_digit()),
            "非法 Pane ID"
        );
        if let Some(pane) = snapshot(client, &id, 0)? {
            panes.push(pane);
        }
    }
    Ok(panes)
}

fn feed_rows(parser: &mut TerminalState, rows: &[Vec<u8>]) {
    for (index, row) in rows.iter().enumerate() {
        if index != 0 {
            parser.process(b"\r\n");
        }
        parser.process(row);
    }
}

fn snapshot(client: &mut Client, id: &str, offset: usize) -> Result<Option<Pane>> {
    let fields = [
        "pane_left",
        "pane_top",
        "pane_width",
        "pane_height",
        "pane_active",
        "window_zoomed_flag",
        "cursor_x",
        "cursor_y",
        "cursor_flag",
    ];
    let format = fields
        .iter()
        .map(|f| format!("#{{{f}}}"))
        .collect::<Vec<_>>()
        .join(" ");
    // 捕获范围在 server 内求值，与状态查询处于同一个命令批次。
    let offset = format!("#{{?#{{e|<:#{{history_size}},{offset}}},#{{history_size}},{offset}}}");
    let start = format!("#{{e|-:0,{offset}}}");
    let end = format!("#{{e|-:#{{e|-:#{{pane_height}},1}},{offset}}}");
    let parts = client.batch(&[
        format!(
            "display-message -p -t {id} '{format} {offset}\t#{{cursor_shape}} #{{cursor_blinking}}\t#{{pane_index}} #{{pane_current_command}}'"
        ),
        format!("capture-pane -p -e -N -S '{start}' -E '{end}' -t {id}"),
    ])?;
    client.events.retain(|event| output_pane(event) != Some(id));
    let state = String::from_utf8(parts[0].concat())?;
    let (state, cursor_style) = state.split_once('\t').context("无法读取 Pane 光标样式")?;
    let (cursor_style, title) = cursor_style.split_once('\t').context("Pane 状态缺少标题")?;
    use crossterm::cursor::SetCursorStyle;
    let cursor_style = match cursor_style.trim() {
        "default 0" | "default 1" => SetCursorStyle::DefaultUserShape,
        "block 0" => SetCursorStyle::SteadyBlock,
        "block 1" => SetCursorStyle::BlinkingBlock,
        "underline 0" => SetCursorStyle::SteadyUnderScore,
        "underline 1" => SetCursorStyle::BlinkingUnderScore,
        "bar 0" => SetCursorStyle::SteadyBar,
        "bar 1" => SetCursorStyle::BlinkingBar,
        _ => anyhow::bail!("无法识别 Pane 光标样式：{cursor_style}"),
    };
    let s = state
        .split_whitespace()
        .map(str::parse::<usize>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(s.len() == fields.len() + 1, "无法读取 Pane 状态：{state}");
    if s[5] == 1 && s[4] != 1 {
        return Ok(None);
    }
    let rect = Rect::new(s[0] as u16, s[1] as u16, s[2] as u16, s[3] as u16);
    let mut parser = TerminalState::new(rect.height, rect.width, 0);
    feed_rows(&mut parser, &parts[1]);
    Ok(Some(Pane {
        id: id.into(),
        rect,
        active: s[4] == 1,
        parser,
        cursor: (s[8] == 1 && s[9] == 0)
            .then_some(((s[6] as u16).min(rect.width.saturating_sub(1)), s[7] as u16)),
        cursor_style,
        scroll_offset: s[9],
        title: title.chars().filter(|c| !c.is_control()).collect(),
    }))
}

fn prepare_window(client: &mut Client) -> Result<()> {
    if client
        .command("show-options -w -v pane-border-status")?
        .concat()
        != b"top"
    {
        client.command("set-option -w pane-border-status top")?;
    }
    Ok(())
}

fn pane_at(panes: &[Pane], x: u16, y: u16) -> Option<&Pane> {
    panes.iter().find(|pane| {
        pane.rect.contains((x, y).into())
            || (pane.rect.y > 0
                && y == pane.rect.y - 1
                && x >= pane.rect.x
                && x < pane.rect.right())
    })
}

struct Drag {
    id: String,
    origin: u16,
    size: u16,
    horizontal: bool,
}
impl Drag {
    fn at(panes: &[Pane], x: u16, y: u16) -> Option<Self> {
        for pane in panes {
            let horizontal = x == pane.rect.right() && y >= pane.rect.y && y < pane.rect.bottom();
            let vertical = y == pane.rect.bottom() && x >= pane.rect.x && x < pane.rect.right();
            let neighbor = panes.iter().any(|other| {
                if horizontal {
                    other.rect.x == x + 1 && y >= other.rect.y && y < other.rect.bottom()
                } else {
                    other.rect.y == y + 1 && x >= other.rect.x && x < other.rect.right()
                }
            });
            if (horizontal || vertical) && neighbor {
                return Some(Self {
                    id: pane.id.clone(),
                    origin: if horizontal { x } else { y },
                    size: if horizontal {
                        pane.rect.width
                    } else {
                        pane.rect.height
                    },
                    horizontal,
                });
            }
        }
        None
    }
    fn command(&self, x: u16, y: u16) -> String {
        let position = if self.horizontal { x } else { y };
        let size = (i32::from(self.size) + i32::from(position) - i32::from(self.origin)).max(1);
        format!(
            "resize-pane -t {} -{} {size}",
            self.id,
            if self.horizontal { 'x' } else { 'y' }
        )
    }
}

fn color(c: AnsiColor) -> Color {
    match c {
        AnsiColor::Named(n) if (n as usize) < 16 => Color::Indexed(n as u8),
        AnsiColor::Named(_) => Color::Reset,
        AnsiColor::Indexed(n) => Color::Indexed(n),
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn draw(frame: &mut ratatui::Frame, panes: &[Pane], status: &str, selection: Option<&Selection>) {
    let area = frame.area();
    let buffer = frame.buffer_mut();
    for pane in panes {
        let border = Style::default().fg(if pane.active {
            Color::Indexed(45)
        } else {
            Color::Indexed(238)
        });
        if pane.rect.x > 0 && pane.rect.x <= area.width {
            for y in
                pane.rect.y..(pane.rect.y + pane.rect.height).min(area.height.saturating_sub(1))
            {
                let edge = if panes.iter().any(|other| {
                    other.active
                        && other.rect.right() == pane.rect.x - 1
                        && y >= other.rect.y
                        && y < other.rect.bottom()
                }) {
                    border.fg(Color::Indexed(45))
                } else {
                    border
                };
                buffer.set_string(pane.rect.x - 1, y, "║", edge);
            }
        }
        if pane.rect.y > 0 && pane.rect.y < area.height {
            for x in pane.rect.x..(pane.rect.x + pane.rect.width).min(area.width) {
                let edge = if panes.iter().any(|other| {
                    other.active
                        && other.rect.bottom() == pane.rect.y - 1
                        && x >= other.rect.x
                        && x < other.rect.right()
                }) {
                    border.fg(Color::Indexed(45))
                } else {
                    border
                };
                buffer.set_string(x, pane.rect.y - 1, "═", edge);
            }
        }
        if pane.rect.y > 0 && pane.rect.y < area.height {
            buffer.set_stringn(
                pane.rect.x,
                pane.rect.y - 1,
                format!(" {} {} ", pane.id, pane.title),
                usize::from(pane.rect.width.min(area.width.saturating_sub(pane.rect.x))),
                border,
            );
        }
        let screen = &pane.parser;
        for y in 0..pane.rect.height {
            for x in 0..pane.rect.width {
                let px = pane.rect.x + x;
                let py = pane.rect.y + y;
                if px >= area.width || py >= area.height.saturating_sub(1) {
                    continue;
                }
                let selected = selection.filter(|selection| selection.pane == pane.id);
                let cell =
                    selected.map_or_else(|| screen.cell(y, x), |selection| selection.cell(y, x));
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut style = Style::default().fg(color(cell.fg)).bg(color(cell.bg));
                for (enabled, modifier) in [
                    (cell.flags.contains(Flags::BOLD), Modifier::BOLD),
                    (cell.flags.contains(Flags::ITALIC), Modifier::ITALIC),
                    (
                        cell.flags.intersects(Flags::ALL_UNDERLINES),
                        Modifier::UNDERLINED,
                    ),
                    (cell.flags.contains(Flags::DIM), Modifier::DIM),
                    (cell.flags.contains(Flags::HIDDEN), Modifier::HIDDEN),
                    (cell.flags.contains(Flags::STRIKEOUT), Modifier::CROSSED_OUT),
                    (cell.flags.contains(Flags::INVERSE), Modifier::REVERSED),
                ] {
                    if enabled {
                        style = style.add_modifier(modifier);
                    }
                }
                if selected.is_some_and(|selection| selection.contains(y, x)) {
                    style = if cell.flags.contains(Flags::INVERSE) {
                        style.remove_modifier(Modifier::REVERSED)
                    } else {
                        style.add_modifier(Modifier::REVERSED)
                    };
                }
                let mut symbol = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    symbol.extend(extra);
                }
                buffer.set_stringn(
                    px,
                    py,
                    if symbol.is_empty() { " " } else { &symbol },
                    usize::from(pane.rect.width - x),
                    style,
                );
            }
        }
    }
    let active = panes.iter().find(|p| p.active);
    let label = format!("{} | {status}", active.map_or("", |p| p.id.as_str()));
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        Rect::new(0, area.height.saturating_sub(1), area.width, 1),
    );
    if let Some(pane) = active
        && selection.is_none_or(|selection| selection.pane != pane.id)
        && let Some((x, y)) = pane.cursor
        && pane.rect.x + x < area.width
        && pane.rect.y + y < area.height.saturating_sub(1)
    {
        frame.set_cursor_position((pane.rect.x + x, pane.rect.y + y));
    }
}

fn key_name(key: event::KeyEvent) -> Option<String> {
    let name = match key.code {
        KeyCode::Enter => "Enter".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BTab".into(),
        KeyCode::Backspace => "BSpace".into(),
        KeyCode::Esc => "Escape".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::PageUp => "PPage".into(),
        KeyCode::PageDown => "NPage".into(),
        KeyCode::F(n) => format!("F{n}"),
        _ => return None,
    };
    let mut prefix = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        prefix.push_str("C-");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        prefix.push_str("M-");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) && key.code != KeyCode::BackTab {
        prefix.push_str("S-");
    }
    Some(format!("{prefix}{name}"))
}

fn key_bytes(key: event::KeyEvent) -> Option<Vec<u8>> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let mut bytes = if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() {
        vec![if c == '?' {
            127
        } else {
            c.to_ascii_uppercase() as u8 & 0x1f
        }]
    } else {
        c.to_string().into_bytes()
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 27);
    }
    Some(bytes)
}

struct CursorStyleGuard;
impl Drop for CursorStyleGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        );
    }
}

struct Screen;
impl Screen {
    fn start() -> Result<Self> {
        enable_raw_mode()?;
        let screen = Self;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

/// 独立实验入口，与 Console 复用同一个终端前端。
pub fn standalone(socket: PathBuf, session: String) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "需要交互终端"
    );
    let _screen = Screen::start()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    run_inner(
        &mut terminal,
        Args {
            socket: Some(socket),
            session,
        },
        None,
        &mut |_, _| Status {
            text: "twig 实验工作台".into(),
            notification: None,
            chat_notification: false,
            unread: Vec::new(),
        },
    )
    .map(|_| ())
}

#[derive(Default, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub notification: Option<crate::notification::CompletionNotice>,
    pub chat_notification: bool,
    pub unread: Vec<UnreadTask>,
}

pub enum Destination {
    Task(crate::notification::CompletionNotice),
    Agent,
}

/// 借用 Console 的终端，不退出 alternate screen，不启动原生 tmux 界面。
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    session: String,
    window: &str,
    mut status: impl FnMut(&str, &str) -> Status,
) -> Result<Option<Destination>> {
    // 防止把正在运行本前端的 Pane 捕获回来，形成递归显示和输入。
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", &pane, "#{session_name}"])
            .output()?;
        ensure!(output.status.success(), "无法确认当前 tmux Pane 的归属");
        ensure!(
            String::from_utf8_lossy(&output.stdout).trim() != session,
            "不能在当前 tmux Session 内打开同一项目工作台；请从其他 Session 或 tmux 外运行 twig"
        );
    }
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    let result = run_inner(
        terminal,
        Args {
            socket: None,
            session,
        },
        Some(window),
        &mut status,
    );
    let restore = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture);
    let outcome = result?;
    restore?;
    Ok(outcome)
}

fn run_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    args: Args,
    window: Option<&str>,
    status: &mut impl FnMut(&str, &str) -> Status,
) -> Result<Option<Destination>> {
    let _cursor_style = CursorStyleGuard;
    let mut client = Client::connect(&args)?;
    if let Some(window) = window {
        ensure!(
            window.starts_with('@') && window[1..].bytes().all(|b| b.is_ascii_digit()),
            "非法窗口 ID"
        );
        client.command(&format!("select-window -t {window}"))?;
    }
    let size = terminal.size()?;
    client.command(&format!(
        "refresh-client -C {},{}",
        size.width,
        size.height.saturating_sub(1)
    ))?;
    prepare_window(&mut client)?;
    let mut panes = reload(&mut client)?;
    let mut window_name =
        client.command("display-message -p '#{window_id}\t#{@twig-feature}\t#{window_name}'")?;
    let mut prefix = false;
    let mut unread_picker: Option<UnreadPicker> = None;
    let mut drag: Option<Drag> = None;
    let mut selection: Option<Selection> = None;
    let mut clicks = crate::mouse_selection::Clicks::default();
    let mut label = Status::default();
    let mut notice = String::new();
    let mut copy_notice: Option<clipboard::Notice> = None;
    let mut dirty = true;
    let mut pending = HashSet::new();
    let mut refreshed = Instant::now();
    let mut refresh_cursor = 0;
    let mut queued_input = None;
    loop {
        if let Some(notice) = clipboard::poll() {
            copy_notice = Some(notice);
            dirty = true;
        }
        if copy_notice.as_ref().is_some_and(clipboard::Notice::expired) {
            copy_notice = None;
            dirty = true;
        }
        client.pump()?;
        client.events.append(&mut client.lines);
        let mut changed = false;
        while let Some(event) = client.events.pop_front() {
            if let Some(id) = output_pane(&event) {
                if panes.iter().any(|p| p.id == id) {
                    pending.insert(id.to_owned());
                }
            } else if event.starts_with(b"%exit") {
                return Ok(None);
            } else if [
                b"%layout-change".as_slice(),
                b"%window-pane-changed",
                b"%session-window-changed",
            ]
            .iter()
            .any(|p| event.starts_with(p))
            {
                changed = true;
            }
        }
        ensure!(!client.closed, "tmux control 连接中断，后台资源未删除");
        if changed {
            prepare_window(&mut client)?;
            panes = reload(&mut client)?;
            window_name = client
                .command("display-message -p '#{window_id}\t#{@twig-feature}\t#{window_name}'")?;
            dirty = true;
        }
        let window_bytes = window_name.concat();
        let window_text = String::from_utf8_lossy(&window_bytes);
        let mut fields = window_text.trim_end_matches('\n').splitn(3, '\t');
        let window_id = fields.next().unwrap_or_default();
        let task = fields.next().unwrap_or_default();
        let name = fields.next().unwrap_or_default();
        let next_label = status(window_id, task);
        if next_label != label {
            label = next_label;
            dirty = true;
        }
        if let Some(picker) = &mut unread_picker {
            picker.sync(&label.unread);
        }
        // 已到达的输入优先；持续输入时仍至少每 100ms 服务一次画面。
        let input_ready = queued_input.is_some() || event::poll(Duration::ZERO)?;
        if refreshed.elapsed() >= Duration::from_millis(33)
            && !pending.is_empty()
            && (!input_ready || refreshed.elapsed() >= Duration::from_millis(100))
        {
            let started = Instant::now();
            for _ in 0..panes.len() {
                let index = refresh_cursor % panes.len();
                refresh_cursor = (index + 1) % panes.len();
                let pane = &mut panes[index];
                if pending.remove(&pane.id)
                    && let Some(updated) = snapshot(&mut client, &pane.id, pane.scroll_offset)?
                {
                    *pane = updated;
                    dirty = true;
                }
                if started.elapsed() >= Duration::from_millis(8) {
                    break;
                }
            }
            pending.retain(|id| panes.iter().any(|p| &p.id == id));
            refreshed = Instant::now();
        }
        if selection.as_ref().is_some_and(|selected| {
            !panes
                .iter()
                .any(|pane| pane.id == selected.pane && pane.rect == selected.rect && pane.active)
        }) {
            selection = None;
            dirty = true;
        }
        if dirty {
            if let Some(pane) = panes.iter().find(|pane| pane.active) {
                execute!(terminal.backend_mut(), pane.cursor_style)?;
            }
            let name = name.chars().filter(|c| !c.is_control()).collect::<String>();
            terminal.draw(|frame| {
                draw(
                    frame,
                    &panes,
                    &format!("{notice}{name} · {}", label.text),
                    selection.as_ref(),
                );
                if let Some(notification) = &label.notification {
                    crate::notification::draw(frame, notification);
                } else if label.chat_notification {
                    crate::notification::draw_chat(frame);
                }
                if let Some(notice) = &copy_notice {
                    notice.draw(frame);
                }
                if let Some(picker) = &unread_picker {
                    picker.draw(frame, &label.unread);
                }
            })?;
            dirty = false;
        }
        if queued_input.is_none() && !event::poll(Duration::from_millis(16))? {
            continue;
        }
        dirty = true;
        let mut input = match queued_input.take() {
            Some(input) => input,
            None => event::read()?,
        };
        // 只合并连续的拖动坐标；松开、按键和粘贴必须保持原顺序。
        if matches!(input, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Drag(MouseButton::Left))
        {
            for _ in 0..64 {
                if !event::poll(Duration::ZERO)? {
                    break;
                }
                let next = event::read()?;
                if matches!(next, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Drag(MouseButton::Left))
                {
                    input = next;
                } else {
                    queued_input = Some(next);
                    break;
                }
            }
        }
        if !matches!(input, Event::Mouse(mouse) if matches!(mouse.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::Moved))
        {
            clicks.reset();
        }
        if matches!(input, Event::Key(_)) {
            notice.clear();
        }
        if let Some(picker) = &mut unread_picker {
            if let Event::Key(key) = input {
                match picker.key(key, &label.unread) {
                    PickerAction::Stay => {}
                    PickerAction::Close => unread_picker = None,
                    PickerAction::Open(notice) => return Ok(Some(Destination::Task(notice))),
                }
            }
            if !matches!(input, Event::Resize(..)) {
                continue;
            }
        }
        match input {
            Event::Resize(width, height) => {
                selection = None;
                drag = None;
                client.command(&format!(
                    "refresh-client -C {width},{}",
                    height.saturating_sub(1)
                ))?;
                panes = reload(&mut client)?;
            }
            Event::Mouse(mouse) => {
                let x = mouse.column;
                let y = mouse.row;
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        selection = None;
                        notice.clear();
                        drag = Drag::at(&panes, x, y);
                        if let Some(pane) = pane_at(&panes, x, y) {
                            if !pane.active {
                                client.command(&format!("select-pane -t {}", pane.id))?;
                            }
                            if drag.is_none() && pane.rect.contains((x, y).into()) {
                                let count = clicks.next(&pane.id, x, y, Instant::now());
                                let mut selected = Selection::new(pane, x, y);
                                selected.capture_history(&mut client, pane.scroll_offset)?;
                                selected.select_click(count);
                                selection = Some(selected);
                            }
                            let id = pane.id.clone();
                            for pane in &mut panes {
                                pane.active = pane.id == id;
                            }
                        }
                        if selection.is_none() {
                            clicks.reset();
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(drag) = &drag {
                            client.command(&drag.command(x, y))?;
                            panes = reload(&mut client)?;
                        } else if let Some(selection) = &mut selection
                            && selection.dragging
                        {
                            selection.update(x, y);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        drag = None;
                        if let Some(selected) = &mut selection
                            && selected.dragging
                        {
                            selected.update(x, y);
                            selected.dragging = false;
                            let text = selected.text();
                            if text.is_empty() {
                                selection = None;
                            } else {
                                copy_notice = Some(clipboard::start(text));
                            }
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        if let Some(selected) = &mut selection
                            && selected.dragging
                        {
                            if selected.rect.contains((x, y).into()) {
                                selected.scroll(mouse.kind == MouseEventKind::ScrollUp, x, y);
                                if let Some(pane) = panes.iter_mut().find(|p| p.id == selected.pane)
                                {
                                    pane.scroll_offset = selected.scroll_offset();
                                    pending.insert(pane.id.clone());
                                }
                            }
                            continue;
                        }
                        selection = None;
                        if let Some(pane) = panes
                            .iter_mut()
                            .find(|pane| pane.rect.contains((x, y).into()))
                        {
                            pane.scroll_offset = if client.wheel(pane, mouse)? {
                                0
                            } else if mouse.kind == MouseEventKind::ScrollUp {
                                pane.scroll_offset + 3
                            } else {
                                pane.scroll_offset.saturating_sub(3)
                            };
                            pending.insert(pane.id.clone());
                        }
                    }
                    MouseEventKind::Moved => {
                        dirty = false;
                    }
                    _ => {}
                }
            }
            Event::Paste(text) => {
                selection = None;
                if let Some(pane) = panes.iter_mut().find(|p| p.active) {
                    pane.scroll_offset = 0;
                    pending.insert(pane.id.clone());
                    client.paste(&pane.id, &text)?;
                }
            }
            Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                if selection.take().is_some() && key.code == KeyCode::Esc {
                    continue;
                }
                if prefix {
                    prefix = false;
                    let command = match key.code {
                        KeyCode::Char('d') => return Ok(None),
                        KeyCode::Char('u') => {
                            unread_picker = Some(UnreadPicker::default());
                            drag = None;
                            None
                        }
                        KeyCode::Char('f') if label.notification.is_some() => {
                            return Ok(label.notification.map(Destination::Task));
                        }
                        KeyCode::Char('f') if label.chat_notification => {
                            return Ok(Some(Destination::Agent));
                        }
                        KeyCode::Char('?') => {
                            prefix = true;
                            None
                        }
                        KeyCode::Char('s') => Some("split-window -h -c '#{pane_current_path}'"),
                        KeyCode::Char('v') => Some("split-window -v -c '#{pane_current_path}'"),
                        KeyCode::Char('o') => Some("select-pane -t :.+"),
                        KeyCode::Char('z') => Some("resize-pane -Z"),
                        KeyCode::Char('n') => Some("next-window"),
                        KeyCode::Char('p') => Some("previous-window"),
                        KeyCode::Char('r') => Some("source-file ~/.tmux.conf"),
                        KeyCode::Char('h') => Some("select-pane -L"),
                        KeyCode::Char('j') => Some("select-pane -D"),
                        KeyCode::Char('k') => Some("select-pane -U"),
                        KeyCode::Char('l') => Some("select-pane -R"),
                        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if let Some(pane) = panes.iter().find(|p| p.active) {
                                client.send(&pane.id, &[23])?;
                            }
                            None
                        }
                        _ => None,
                    };
                    if let Some(command) = command {
                        client.command(command)?;
                        if key.code == KeyCode::Char('r') {
                            notice = "tmux.conf 已重载 · ".into();
                        }
                        panes = reload(&mut client)?;
                    }
                } else if key.code == KeyCode::Char('w')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    prefix = true;
                } else if key.modifiers == KeyModifiers::ALT
                    && matches!(key.code, KeyCode::Char('h' | 'j' | 'k' | 'l'))
                {
                    let direction = match key.code {
                        KeyCode::Char('h') => 'L',
                        KeyCode::Char('j') => 'D',
                        KeyCode::Char('k') => 'U',
                        _ => 'R',
                    };
                    client.command(&format!("resize-pane -{direction} 3"))?;
                    panes = reload(&mut client)?;
                } else if let Some(pane) = panes.iter_mut().find(|p| p.active) {
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
                    {
                        let offset = pane.scroll_offset;
                        pane.scroll_offset = if key.code == KeyCode::PageUp {
                            offset + 10
                        } else {
                            offset.saturating_sub(10)
                        };
                        pending.insert(pane.id.clone());
                    } else {
                        pane.scroll_offset = 0;
                        pending.insert(pane.id.clone());
                        client.key(&pane.id, key)?;
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_protocols_preserve_coordinates_and_modifiers() {
        let rect = Rect::new(10, 4, 400, 30);
        let mut mouse = event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 11,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(wheel_bytes(mouse, rect, true, false), b"\x1b[<64;2;3M");
        assert_eq!(wheel_bytes(mouse, rect, false, false), b"\x1b[M`\x22\x23");
        mouse.kind = MouseEventKind::ScrollDown;
        mouse.modifiers = KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL;
        assert_eq!(wheel_bytes(mouse, rect, true, true), b"\x1b[<93;2;3M");
        mouse.column = 309;
        assert_eq!(
            wheel_bytes(mouse, rect, false, true),
            "\x1b[M}\u{14c}#".as_bytes()
        );
        assert_eq!(wheel_bytes(mouse, rect, false, false), b"\x1b[M}\xff#");
        mouse.column = 2025;
        assert!(wheel_bytes(mouse, rect, false, true).is_empty());
    }

    fn visible_text(text: &str) -> String {
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end_matches('\n')
            .into()
    }

    struct Server {
        _dir: tempfile::TempDir,
        args: Args,
    }
    impl Server {
        fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let args = Args {
                socket: Some(dir.path().join("tmux.sock")),
                session: "test".into(),
            };
            let output = Command::new("tmux")
                .arg("-S")
                .arg(args.socket.as_ref().unwrap())
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "test",
                    "-x",
                    "80",
                    "-y",
                    "24",
                    "env PS1='PROMPT> ' /bin/sh",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self { _dir: dir, args }
        }
        fn capture(&self, pane: &str) -> String {
            let out = Command::new("tmux")
                .arg("-S")
                .arg(self.args.socket.as_ref().unwrap())
                .args(["capture-pane", "-p", "-t", pane])
                .output()
                .unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        }
        fn stream() -> (Self, std::fs::File) {
            use std::{ffi::CString, fs::OpenOptions, os::unix::ffi::OsStrExt};
            let dir = tempfile::tempdir().unwrap();
            let fifo = dir.path().join("output");
            let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
            // SAFETY: name 是存活的 NUL 结尾路径；FIFO 仅在本测试临时目录中。
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let stream = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&fifo)
                .unwrap();
            let args = Args {
                socket: Some(dir.path().join("tmux.sock")),
                session: "test".into(),
            };
            let out = Command::new("tmux")
                .arg("-S")
                .arg(args.socket.as_ref().unwrap())
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "test",
                    "-x",
                    "80",
                    "-y",
                    "24",
                    "/bin/cat",
                ])
                .arg(&fifo)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            (Self { _dir: dir, args }, stream)
        }
        fn wait_for(&self, pane: &str, needle: &str) {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let text = self.capture(pane);
                if text.contains(needle) {
                    return;
                }
                assert!(Instant::now() < until, "缺少 {needle}: {text}");
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(self.args.socket.as_ref().unwrap())
                .arg("kill-server")
                .output();
        }
    }

    fn drain_output(client: &mut Client, pane: &mut Pane, needle: &str) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            client.pump().unwrap();
            client.events.append(&mut client.lines);
            while let Some(event) = client.events.pop_front() {
                if output_pane(&event) == Some(pane.id.as_str()) {
                    *pane = snapshot(client, &pane.id, pane.scroll_offset)
                        .unwrap()
                        .unwrap();
                }
            }
            if pane.parser.text().contains(needle) {
                return;
            }
            assert!(
                Instant::now() < until,
                "缺少 {needle}: {}",
                pane.parser.text()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn server_encodes_cursor_keys_and_bracketed_paste_without_snapshot_races() {
        let server = Server::start();
        let mut client = Client::connect(&server.args).unwrap();
        let destination = server._dir.path().join("input");
        client
            .send(
                "%0",
                format!(
                    "stty raw -echo; printf '\\033[?1h\\033[?2004h'; cat > '{}'\r",
                    destination.display()
                )
                .as_bytes(),
            )
            .unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while !destination.exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        client
            .key("%0", event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .unwrap();
        let text = format!("中文\n$HOME '\"; kill-server\n{}", "文本".repeat(800));
        client.paste("%0", &text).unwrap();
        let expected = format!("\x1bOA\x1b[200~{text}\x1b[201~");
        loop {
            let received = std::fs::read(&destination).unwrap();
            if received.len() >= expected.len() {
                assert_eq!(received, expected.as_bytes());
                break;
            }
            assert!(Instant::now() < until, "输入未到达");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            client
                .command("list-buffers -F '#{buffer_name}'")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reconnect_restores_alternate_screen_and_returns_to_primary() {
        let (server, mut output) = Server::stream();
        for i in 0..60 {
            writeln!(output, "PRIMARY_HISTORY_{i:03}\r").unwrap();
        }
        output
            .write_all(b"PRIMARY_MARKER\r\n\x1b[?1049h\x1b[H\x1b[2JALT_MARKER")
            .unwrap();
        server.wait_for("%0", "ALT_MARKER");
        let mut client = Client::connect(&server.args).unwrap();
        let mut pane = reload(&mut client).unwrap().remove(0);
        assert!(pane.parser.text().contains("ALT_MARKER"));
        output.write_all(b"\x1b[?1049lRETURNED").unwrap();
        server.wait_for("%0", "RETURNED");
        drain_output(&mut client, &mut pane, "RETURNED");
        assert!(pane.parser.text().contains("PRIMARY_MARKER"));
        assert_eq!(
            visible_text(&pane.parser.text()),
            visible_text(&server.capture("%0"))
        );
        pane = snapshot(&mut client, "%0", 2000).unwrap().unwrap();
        assert!(pane.parser.text().contains("PRIMARY_HISTORY_000"));
    }

    #[test]
    fn reconnect_keeps_history_and_pending_escape_sequence() {
        let (server, mut output) = Server::stream();
        for i in 0..60 {
            writeln!(output, "ROW_{i:03}\r").unwrap();
        }
        output.write_all(b"BEFORE_PENDING\x1b[").unwrap();
        server.wait_for("%0", "BEFORE_PENDING");
        let mut client = Client::connect(&server.args).unwrap();
        let mut pane = snapshot(&mut client, "%0", 50).unwrap().unwrap();
        assert!(pane.parser.text().contains("ROW_000"));
        pane = snapshot(&mut client, "%0", 0).unwrap().unwrap();
        output.write_all(b"31mRED_MARKER").unwrap();
        server.wait_for("%0", "RED_MARKER");
        drain_output(&mut client, &mut pane, "RED_MARKER");
        let (x, y) = pane.cursor.unwrap();
        assert_eq!(
            pane.parser.cell(y, x - 1).fg,
            AnsiColor::Named(alacritty_terminal::vte::ansi::NamedColor::Red)
        );
        assert_eq!(
            visible_text(&pane.parser.text()),
            visible_text(&server.capture("%0"))
        );
    }

    #[test]
    fn reconnect_uses_server_drawing_attributes() {
        let (server, mut output) = Server::stream();
        output.write_all(b"UNCHANGED\x1b[31m").unwrap();
        server.wait_for("%0", "UNCHANGED");
        let mut client = Client::connect(&server.args).unwrap();
        let mut pane = reload(&mut client).unwrap().remove(0);
        output.write_all(b"RED").unwrap();
        server.wait_for("%0", "UNCHANGEDRED");
        drain_output(&mut client, &mut pane, "UNCHANGEDRED");
        assert_eq!(
            pane.parser.cell(0, 9).fg,
            AnsiColor::Named(alacritty_terminal::vte::ansi::NamedColor::Red)
        );
    }

    #[test]
    fn reconnect_handles_all_tmux_alternate_screen_modes() {
        for mode in [47, 1047, 1049] {
            let (server, mut output) = Server::stream();
            write!(output, "PRIMARY\x1b[?{mode}h\x1b[H\x1b[2JALTERNATE").unwrap();
            server.wait_for("%0", "ALTERNATE");
            let mut client = Client::connect(&server.args).unwrap();
            let mut pane = reload(&mut client).unwrap().remove(0);
            assert!(pane.parser.text().contains("ALTERNATE"));
            write!(output, "\x1b[?{mode}l\r\nRETURNED").unwrap();
            server.wait_for("%0", "RETURNED");
            drain_output(&mut client, &mut pane, "RETURNED");
            assert!(pane.parser.text().contains("PRIMARY"), "mode {mode}");
            assert_eq!(pane.parser.text(), visible_text(&server.capture("%0")));
        }
    }

    #[test]
    fn history_view_clamps_after_server_history_is_cleared() {
        let (server, mut output) = Server::stream();
        for i in 0..60 {
            writeln!(output, "ROW_{i:03}\r").unwrap();
        }
        output.write_all(b"DONE").unwrap();
        server.wait_for("%0", "DONE");
        let mut client = Client::connect(&server.args).unwrap();
        let pane = snapshot(&mut client, "%0", 2000).unwrap().unwrap();
        assert!(pane.parser.text().starts_with("ROW_000"));
        assert!(pane.cursor.is_none());
        client.command("clear-history -t %0").unwrap();
        let pane = snapshot(&mut client, "%0", pane.scroll_offset)
            .unwrap()
            .unwrap();
        assert_eq!(pane.scroll_offset, 0);
        assert_eq!(pane.parser.text(), visible_text(&server.capture("%0")));
    }

    #[test]
    fn snapshots_during_output_do_not_duplicate_or_drop_stream_bytes() {
        let (server, mut output) = Server::stream();
        output.write_all(b"BEGIN\r\n").unwrap();
        server.wait_for("%0", "BEGIN");
        let mut client = Client::connect(&server.args).unwrap();
        let producer = thread::spawn(move || {
            for i in 0..80 {
                write!(output, "{i:03}\r\n").unwrap();
                thread::sleep(Duration::from_millis(2));
            }
            output.write_all(b"STREAM_DONE").unwrap();
            output
        });
        let mut pane = reload(&mut client).unwrap().remove(0);
        for _ in 0..8 {
            pane = snapshot(&mut client, "%0", 0).unwrap().unwrap();
        }
        let _output = producer.join().unwrap();
        server.wait_for("%0", "STREAM_DONE");
        drain_output(&mut client, &mut pane, "STREAM_DONE");
        assert_eq!(
            visible_text(&pane.parser.text()),
            visible_text(&server.capture("%0"))
        );
        pane = snapshot(&mut client, "%0", 2000).unwrap().unwrap();
        assert!(
            pane.parser.text().starts_with("BEGIN\n000\n001\n002"),
            "{}",
            pane.parser.text()
        );
    }

    #[test]
    fn reconnect_restores_insert_mode_and_pending_wrap() {
        let (server, mut output) = Server::stream();
        output.write_all(b"abc\r\x1b[4h").unwrap();
        server.wait_for("%0", "abc");
        let mut client = Client::connect(&server.args).unwrap();
        let mut pane = reload(&mut client).unwrap().remove(0);
        output.write_all(b"X\x1b[4l\r\n").unwrap();
        server.wait_for("%0", "Xabc");
        drain_output(&mut client, &mut pane, "Xabc");
        output.write_all(&[b'A'; 80]).unwrap();
        server.wait_for("%0", &"A".repeat(80));
        pane = snapshot(&mut client, "%0", 0).unwrap().unwrap();
        output.write_all(b"WRAPPED").unwrap();
        server.wait_for("%0", "WRAPPED");
        drain_output(&mut client, &mut pane, "WRAPPED");
        assert_eq!(
            visible_text(&pane.parser.text()),
            visible_text(&server.capture("%0"))
        );
    }

    #[test]
    fn top_headers_and_drag_geometry_match_real_tmux_layouts() {
        let server = Server::start();
        let mut client = Client::connect(&server.args).unwrap();
        prepare_window(&mut client).unwrap();
        client.command("split-window -h").unwrap();
        let panes = reload(&mut client).unwrap();
        let left = panes.iter().min_by_key(|p| p.rect.x).unwrap();
        assert_eq!(left.rect.y, 1);
        assert_eq!(pane_at(&panes, left.rect.x, 0).unwrap().id, left.id);
        let drag = Drag::at(&panes, left.rect.right(), left.rect.y + 1).unwrap();
        client
            .command(&drag.command(left.rect.right() + 3, left.rect.y + 1))
            .unwrap();
        let updated = snapshot(&mut client, &left.id, 0).unwrap().unwrap();
        assert_eq!(updated.rect.width, left.rect.width + 3);
        client
            .command(&format!("split-window -v -t {}", left.id))
            .unwrap();
        let panes = reload(&mut client).unwrap();
        let upper = panes.iter().find(|p| p.id == left.id).unwrap();
        let drag = Drag::at(&panes, upper.rect.x + 1, upper.rect.bottom()).unwrap();
        client
            .command(&drag.command(upper.rect.x + 1, upper.rect.bottom() + 2))
            .unwrap();
        let updated = snapshot(&mut client, &upper.id, 0).unwrap().unwrap();
        assert_eq!(updated.rect.height, upper.rect.height + 2);
    }

    #[test]
    fn real_server_split_resize_detach_and_reconnect() {
        let server = Server::start();
        let mut client = Client::connect(&server.args).unwrap();
        client.command("refresh-client -C 90,25").unwrap();
        let panes = reload(&mut client).unwrap();
        assert_eq!(panes.len(), 1);
        let first = panes[0].id.clone();
        server.wait_for(&first, "PROMPT>");
        client
            .send(
                &first,
                "printf '\\033[31m%s\\033[0m\\n' '重连标记'\r".as_bytes(),
            )
            .unwrap();
        server.wait_for(&first, "重连标记");
        client.command("split-window -h").unwrap();
        let panes = reload(&mut client).unwrap();
        assert_eq!(panes.len(), 2);
        assert!(panes.iter().all(|p| p.rect.width < 90));
        client.command("resize-pane -Z").unwrap();
        let zoomed = reload(&mut client).unwrap();
        assert_eq!(zoomed.len(), 1);
        assert_eq!(zoomed[0].rect.width, 90);
        client.command("resize-pane -Z").unwrap();
        client.command("refresh-client -C 100,30").unwrap();
        let panes = reload(&mut client).unwrap();
        assert_eq!(panes.iter().map(|p| p.rect.width).sum::<u16>() + 1, 100);
        assert!(panes.iter().all(|p| p.rect.height == 30));
        let pids = client.command("list-panes -F '#{pane_pid}'").unwrap();
        drop(client);
        let mut client = Client::connect(&server.args).unwrap();
        assert_eq!(client.command("list-panes -F '#{pane_pid}'").unwrap(), pids);
        let panes = reload(&mut client).unwrap();
        assert!(panes.iter().any(|p| p.parser.text().contains("重连标记")));
        // 任意字符作为输入传递，不能逃逸为 tmux 命令。
        client
            .send(&first, b"printf '%s\\n' '; kill-server'\r")
            .unwrap();
        server.wait_for(&first, "; kill-server");
        assert!(
            client
                .command("display-message -p '#{session_name}'")
                .is_ok()
        );
    }

    #[test]
    fn output_notifications_only_identify_dirty_panes() {
        assert_eq!(output_pane(b"%output %3 hi\\015\\033[31m"), Some("%3"));
        assert_eq!(output_pane(b"%layout-change @1"), None);
    }

    #[test]
    fn keyboard_uses_tmux_names_and_preserves_characters() {
        assert_eq!(
            key_name(event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some("Up".into())
        );
        assert_eq!(
            key_name(event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            Some("C-Left".into())
        );
        assert_eq!(
            key_name(event::KeyEvent::new(KeyCode::F(5), KeyModifiers::ALT)),
            Some("M-F5".into())
        );
        assert_eq!(
            key_bytes(event::KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))
            .unwrap(),
            [3]
        );
        assert_eq!(
            key_bytes(event::KeyEvent::new(
                KeyCode::Char('中'),
                KeyModifiers::NONE
            ))
            .unwrap(),
            "中".as_bytes()
        );
    }

    #[test]
    fn renderer_keeps_chinese_colors_and_pane_coordinates() {
        let mut parser = TerminalState::new(3, 12, 10);
        parser.process("\x1b[31m中文\x1b[0m ok".as_bytes());
        let pane = Pane {
            id: "%0".into(),
            rect: Rect::new(2, 1, 12, 3),
            active: true,
            parser,
            cursor: None,
            cursor_style: crossterm::cursor::SetCursorStyle::DefaultUserShape,
            scroll_offset: 0,
            title: "shell".into(),
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        terminal
            .draw(|frame| draw(frame, &[pane], "twig", None))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "中");
        assert_eq!(buffer[(4, 1)].symbol(), "文");
        assert_eq!(buffer[(2, 1)].fg, Color::Indexed(1));
        assert_eq!(buffer[(1, 2)].symbol(), "║");
        assert_eq!(buffer[(1, 2)].fg, Color::Indexed(45));
        assert_eq!(buffer[(2, 0)].fg, Color::Indexed(45));
    }
}
