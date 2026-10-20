//! 独立终端原型；只连接显式指定的已有 tmux socket/session。
//! cargo run --example control_mode -- --socket /tmp/demo.sock --session demo
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
    collections::VecDeque,
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
        writeln!(self.input, "{command}")?;
        self.input.flush()?;
        self.response()
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

fn decode_output(line: &[u8]) -> Result<Option<(String, Vec<u8>)>> {
    let Some(rest) = line.strip_prefix(b"%output ") else {
        return Ok(None);
    };
    let split = rest
        .iter()
        .position(|&b| b == b' ')
        .context("输出缺少 Pane")?;
    let pane = String::from_utf8(rest[..split].to_vec())?;
    let mut bytes = &rest[split + 1..];
    let mut output = Vec::new();
    while let Some((&first, tail)) = bytes.split_first() {
        if first == b'\\' {
            ensure!(
                tail.len() >= 3 && tail[..3].iter().all(|b| (b'0'..=b'7').contains(b)),
                "非法 control 八进制转义"
            );
            let value = (tail[0] - b'0') as u16 * 64
                + (tail[1] - b'0') as u16 * 8
                + (tail[2] - b'0') as u16;
            output.push(u8::try_from(value)?);
            bytes = &tail[3..];
        } else {
            output.push(first);
            bytes = tail;
        }
    }
    Ok(Some((pane, output)))
}

struct Pane {
    id: String,
    rect: Rect,
    active: bool,
    parser: vt100::Parser,
}

fn reload(client: &mut Client) -> Result<Vec<Pane>> {
    let lines = client.command(
        "list-panes -F '#{pane_id} #{pane_left} #{pane_top} #{pane_width} #{pane_height} #{pane_active} #{window_zoomed_flag}'",
    )?;
    let mut panes = Vec::new();
    for line in lines {
        let text = String::from_utf8(line)?;
        let f: Vec<_> = text.split_whitespace().collect();
        ensure!(f.len() == 7, "非法 Pane 布局：{text}");
        if f[6] == "1" && f[5] != "1" {
            continue;
        }
        ensure!(
            f[0].starts_with('%') && f[0][1..].bytes().all(|b| b.is_ascii_digit()),
            "非法 Pane ID"
        );
        let rect = Rect::new(f[1].parse()?, f[2].parse()?, f[3].parse()?, f[4].parse()?);
        let mut parser = vt100::Parser::new(rect.height, rect.width, 2000);
        // 先捕获屏幕再衔接实时输出；捕获前排队的此 Pane 输出不能重复播放。
        let snapshot = client.command(&format!("capture-pane -p -e -N -t {}", f[0]))?;
        client
            .events
            .retain(|event| !event.starts_with(format!("%output {} ", f[0]).as_bytes()));
        for (index, row) in snapshot.iter().enumerate() {
            if index != 0 {
                parser.process(b"\r\n");
            }
            parser.process(row);
        }
        // 此原型只恢复可查询的常用模式；不能将截图等同于完整终端状态。
        let state = client.command(&format!(
            "display-message -p -t {} '#{{cursor_x}} #{{cursor_y}} #{{cursor_flag}} #{{keypad_cursor_flag}} #{{bracket_paste_flag}}'",
            f[0]
        ))?;
        let state = String::from_utf8(state.concat())?;
        let s: Vec<_> = state.split_whitespace().collect();
        ensure!(s.len() == 5, "无法恢复 Pane 光标模式：{state}");
        parser.process(
            format!(
                "\x1b[{};{}H\x1b[?25{}\x1b[?1{}\x1b[?2004{}",
                s[1].parse::<u16>()? + 1,
                s[0].parse::<u16>()? + 1,
                if s[2] == "1" { 'h' } else { 'l' },
                if s[3] == "1" { 'h' } else { 'l' },
                if s[4] == "1" { 'h' } else { 'l' }
            )
            .as_bytes(),
        );
        panes.push(Pane {
            id: f[0].into(),
            rect,
            active: f[5] == "1",
            parser,
        });
    }
    Ok(panes)
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(n) => Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn draw(frame: &mut ratatui::Frame, panes: &[Pane], prefix: bool) {
    let area = frame.area();
    let buffer = frame.buffer_mut();
    for pane in panes {
        let screen = pane.parser.screen();
        for y in 0..pane.rect.height {
            for x in 0..pane.rect.width {
                let px = pane.rect.x + x;
                let py = pane.rect.y + y;
                if px >= area.width || py >= area.height.saturating_sub(1) {
                    continue;
                }
                let Some(cell) = screen.cell(y, x) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = Style::default()
                    .fg(color(cell.fgcolor()))
                    .bg(color(cell.bgcolor()));
                for (enabled, modifier) in [
                    (cell.bold(), Modifier::BOLD),
                    (cell.italic(), Modifier::ITALIC),
                    (cell.underline(), Modifier::UNDERLINED),
                    (cell.inverse(), Modifier::REVERSED),
                ] {
                    if enabled {
                        style = style.add_modifier(modifier);
                    }
                }
                let symbol = cell.contents();
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
    if let Some(pane) = active {
        let screen = pane.parser.screen();
        let (y, x) = screen.cursor_position();
        if !screen.hide_cursor()
            && screen.scrollback() == 0
            && pane.rect.x + x < area.width
            && pane.rect.y + y < area.height.saturating_sub(1)
        {
            frame.set_cursor_position((pane.rect.x + x, pane.rect.y + y));
        }
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
    loop {
        client.pump()?;
        client.events.append(&mut client.lines);
        let mut changed = false;
        while let Some(event) = client.events.pop_front() {
            if let Some((id, bytes)) = decode_output(&event)? {
                if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                    pane.parser.process(&bytes);
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
        }
        terminal.draw(|frame| draw(frame, &panes, prefix))?;
        if !event::poll(Duration::from_millis(16))? {
            continue;
        }
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
                    pane.parser.set_scrollback(0);
                    let bytes = if pane.parser.screen().bracketed_paste() {
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
                        let offset = pane.parser.screen().scrollback();
                        pane.parser.set_scrollback(if key.code == KeyCode::PageUp {
                            offset + 10
                        } else {
                            offset.saturating_sub(10)
                        });
                    } else if let Some(bytes) =
                        key_bytes(key, pane.parser.screen().application_cursor())
                    {
                        pane.parser.set_scrollback(0);
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
                .status();
        }
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
        assert!(
            panes
                .iter()
                .any(|p| p.parser.screen().contents().contains("重连标记"))
        );
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
    fn output_preserves_bytes_and_escape_boundaries() {
        let (pane, data) = decode_output(b"%output %3 hi\\015\\012\\033[31m\\134\\377")
            .unwrap()
            .unwrap();
        assert_eq!(pane, "%3");
        assert_eq!(data, b"hi\r\n\x1b[31m\\\xff");
        assert!(decode_output(b"%output %1 \\9").is_err());
        assert!(decode_output(b"%output %1 \\777").is_err());
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
        let mut parser = vt100::Parser::new(3, 12, 10);
        parser.process("\x1b[31m中文\x1b[0m ok".as_bytes());
        let pane = Pane {
            id: "%0".into(),
            rect: Rect::new(2, 1, 12, 3),
            active: true,
            parser,
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 8)).unwrap();
        terminal.draw(|frame| draw(frame, &[pane], false)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "中");
        assert_eq!(buffer[(4, 1)].symbol(), "文");
        assert_eq!(buffer[(2, 1)].fg, Color::Indexed(1));
    }
}
