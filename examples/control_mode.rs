//! 独立终端原型；只连接显式指定的已有 tmux socket/session。
//! cargo run --example control_mode -- --socket /tmp/demo.sock --session demo
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
use clap::Parser;
use crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers},
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
use std::{
    collections::{HashSet, VecDeque},
    io::{self, IsTerminal, Read, Write},
    os::fd::AsRawFd,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(about = "Control Mode 实验前端；连接已有会话，退出保留进程")]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    session: String,
}

struct Client {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    bytes: Vec<u8>,
    lines: VecDeque<Vec<u8>>,
    events: VecDeque<Vec<u8>>,
}

impl Client {
    fn connect(args: &Args) -> Result<Self> {
        // has-session 不启动 server，也不隐式创建会话。
        let status = Command::new("tmux")
            .arg("-S")
            .arg(&args.socket)
            .args(["has-session", "-t", &format!("={}", args.session)])
            .status()?;
        ensure!(status.success(), "指定的 tmux 会话不存在");
        let mut child = Command::new("tmux")
            .arg("-S")
            .arg(&args.socket)
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
                Ok(0) => bail!("tmux control 连接已关闭，后台资源未删除"),
                Ok(n) => self.bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        ensure!(self.bytes.len() < 4 * 1024 * 1024, "control 单行输出过大");
        while let Some(end) = self.bytes.iter().position(|&b| b == b'\n') {
            let mut line: Vec<_> = self.bytes.drain(..=end).collect();
            line.pop();
            self.lines.push_back(line);
        }
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
                        "control 事件积压过多，原型停止读取"
                    );
                }
            }
            ensure!(Instant::now() < until, "等待 tmux 应答超时");
            self.pump()?;
            thread::sleep(Duration::from_millis(2));
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
    scroll_offset: usize,
    app_cursor: bool,
    bracketed_paste: bool,
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
        "keypad_cursor_flag",
        "bracket_paste_flag",
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
        format!("display-message -p -t {id} '{format} {offset}'"),
        format!("capture-pane -p -e -N -S '{start}' -E '{end}' -t {id}"),
    ])?;
    client.events.retain(|event| output_pane(event) != Some(id));
    let state = String::from_utf8(parts[0].concat())?;
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
        cursor: (s[8] == 1 && s[11] == 0)
            .then_some(((s[6] as u16).min(rect.width.saturating_sub(1)), s[7] as u16)),
        scroll_offset: s[11],
        app_cursor: s[9] == 1,
        bracketed_paste: s[10] == 1,
    }))
}

fn color(c: AnsiColor) -> Color {
    match c {
        AnsiColor::Named(n) if (n as usize) < 16 => Color::Indexed(n as u8),
        AnsiColor::Named(_) => Color::Reset,
        AnsiColor::Indexed(n) => Color::Indexed(n),
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn draw(frame: &mut ratatui::Frame, panes: &[Pane], prefix: bool) {
    let area = frame.area();
    let buffer = frame.buffer_mut();
    for pane in panes {
        let screen = &pane.parser;
        for y in 0..pane.rect.height {
            for x in 0..pane.rect.width {
                let px = pane.rect.x + x;
                let py = pane.rect.y + y;
                if px >= area.width || py >= area.height.saturating_sub(1) {
                    continue;
                }
                let cell = screen.cell(y, x);
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut style = Style::default().fg(color(cell.fg)).bg(color(cell.bg));
                for (enabled, modifier) in [
                    (cell.flags.contains(Flags::BOLD), Modifier::BOLD),
                    (cell.flags.contains(Flags::ITALIC), Modifier::ITALIC),
                    (cell.flags.contains(Flags::UNDERLINE), Modifier::UNDERLINED),
                    (cell.flags.contains(Flags::INVERSE), Modifier::REVERSED),
                ] {
                    if enabled {
                        style = style.add_modifier(modifier);
                    }
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
    let label = if prefix {
        "Ctrl-G: d 退出 | % 左右分屏 | \" 上下分屏 | o 切 Pane | z 放大 | n/p 窗口 | h/j/k/l 调比例 | Esc 取消".to_string()
    } else {
        format!(
            "twig 原型 | {} | Ctrl-G 操作 | Shift-PgUp/PgDn 历史",
            active.map_or("", |p| p.id.as_str())
        )
    };
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        Rect::new(0, area.height.saturating_sub(1), area.width, 1),
    );
    if let Some(pane) = active
        && let Some((x, y)) = pane.cursor
        && pane.rect.x + x < area.width
        && pane.rect.y + y < area.height.saturating_sub(1)
    {
        frame.set_cursor_position((pane.rect.x + x, pane.rect.y + y));
    }
}

fn key_bytes(key: event::KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl && c.is_ascii() => vec![(c.to_ascii_uppercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![127],
        KeyCode::Esc => vec![27],
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Right
        | KeyCode::Left
        | KeyCode::Home
        | KeyCode::End => {
            let suffix = match key.code {
                KeyCode::Up => 'A',
                KeyCode::Down => 'B',
                KeyCode::Right => 'C',
                KeyCode::Left => 'D',
                KeyCode::Home => 'H',
                _ => 'F',
            };
            let mods = 1
                + u8::from(key.modifiers.contains(KeyModifiers::SHIFT))
                + 2 * u8::from(key.modifiers.contains(KeyModifiers::ALT))
                + 4 * u8::from(ctrl);
            if mods > 1 {
                format!("\x1b[1;{mods}{suffix}").into_bytes()
            } else {
                format!("\x1b{}{suffix}", if application_cursor { 'O' } else { '[' }).into_bytes()
            }
        }
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) && matches!(key.code, KeyCode::Char(_)) {
        bytes.insert(0, 27);
    }
    Some(bytes)
}

struct Screen;
impl Screen {
    fn start() -> Result<Self> {
        enable_raw_mode()?;
        let screen = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)?;
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

fn run(args: Args) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "需要交互终端"
    );
    let mut client = Client::connect(&args)?;
    let _screen = Screen::start()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let size = terminal.size()?;
    client.command(&format!(
        "refresh-client -C {},{}",
        size.width,
        size.height.saturating_sub(1)
    ))?;
    let mut panes = reload(&mut client)?;
    let mut prefix = false;
    let mut dirty = true;
    let mut pending = HashSet::new();
    let mut refreshed = Instant::now();
    loop {
        client.pump()?;
        client.events.append(&mut client.lines);
        let mut changed = false;
        while let Some(event) = client.events.pop_front() {
            if let Some(id) = output_pane(&event) {
                if panes.iter().any(|p| p.id == id) {
                    pending.insert(id.to_owned());
                }
            } else if event.starts_with(b"%exit") {
                return Ok(());
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
        if changed {
            panes = reload(&mut client)?;
            dirty = true;
        }
        if refreshed.elapsed() >= Duration::from_millis(33) && !pending.is_empty() {
            for pane in &mut panes {
                if pending.remove(&pane.id)
                    && let Some(updated) = snapshot(&mut client, &pane.id, pane.scroll_offset)?
                {
                    *pane = updated;
                }
            }
            pending.clear();
            refreshed = Instant::now();
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| draw(frame, &panes, prefix))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(16))? {
            continue;
        }
        dirty = true;
        match event::read()? {
            Event::Resize(width, height) => {
                client.command(&format!(
                    "refresh-client -C {width},{}",
                    height.saturating_sub(1)
                ))?;
                panes = reload(&mut client)?;
            }
            Event::Paste(text) => {
                if let Some(pane) = panes.iter_mut().find(|p| p.active) {
                    pane.scroll_offset = 0;
                    pending.insert(pane.id.clone());
                    let bytes = if pane.bracketed_paste {
                        format!("\x1b[200~{text}\x1b[201~")
                    } else {
                        text
                    };
                    client.send(&pane.id, bytes.as_bytes())?;
                }
            }
            Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                if prefix {
                    prefix = false;
                    let command = match key.code {
                        KeyCode::Char('d') => return Ok(()),
                        KeyCode::Char('%') => Some("split-window -h -c '#{pane_current_path}'"),
                        KeyCode::Char('"') => Some("split-window -v -c '#{pane_current_path}'"),
                        KeyCode::Char('o') => Some("select-pane -t :.+"),
                        KeyCode::Char('z') => Some("resize-pane -Z"),
                        KeyCode::Char('n') => Some("next-window"),
                        KeyCode::Char('p') => Some("previous-window"),
                        KeyCode::Char('h') => Some("resize-pane -L 3"),
                        KeyCode::Char('j') => Some("resize-pane -D 2"),
                        KeyCode::Char('k') => Some("resize-pane -U 2"),
                        KeyCode::Char('l') => Some("resize-pane -R 3"),
                        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if let Some(pane) = panes.iter().find(|p| p.active) {
                                client.send(&pane.id, &[7])?;
                            }
                            None
                        }
                        _ => None,
                    };
                    if let Some(command) = command {
                        client.command(command)?;
                        panes = reload(&mut client)?;
                    }
                } else if key.code == KeyCode::Char('g')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    prefix = true;
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
                    } else if let Some(bytes) = key_bytes(key, pane.app_cursor) {
                        pane.scroll_offset = 0;
                        pending.insert(pane.id.clone());
                        client.send(&pane.id, &bytes)?;
                    }
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        drop(Screen);
        previous(info);
    }));
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                socket: dir.path().join("tmux.sock"),
                session: "test".into(),
            };
            let output = Command::new("tmux")
                .arg("-S")
                .arg(&args.socket)
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
                .arg(&self.args.socket)
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
                socket: dir.path().join("tmux.sock"),
                session: "test".into(),
            };
            let out = Command::new("tmux")
                .arg("-S")
                .arg(&args.socket)
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
                .arg(&self.args.socket)
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
    fn keyboard_respects_cursor_mode_and_modifiers() {
        let key = event::KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(key_bytes(key, true).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(key, false).unwrap(), b"\x1b[A");
        assert_eq!(
            key_bytes(
                event::KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
                false
            )
            .unwrap(),
            b"\x1b[1;5D"
        );
        assert_eq!(
            key_bytes(
                event::KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false
            )
            .unwrap(),
            [3]
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
            scroll_offset: 0,
            app_cursor: false,
            bracketed_paste: false,
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        terminal.draw(|frame| draw(frame, &[pane], false)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "中");
        assert_eq!(buffer[(4, 1)].symbol(), "文");
        assert_eq!(buffer[(2, 1)].fg, Color::Indexed(1));
    }
}
