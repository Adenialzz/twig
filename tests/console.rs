mod support;

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    registry: PathBuf,
    bin: PathBuf,
    socket: String,
    tmux: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let repo = temp.path().join("repo");
        let bin = temp.path().join("bin");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&bin).unwrap();
        let tmux = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v tmux"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        assert!(!tmux.is_empty(), "需要 tmux");
        let socket = format!(
            "twig-console-{}-{}",
            std::process::id(),
            temp.path().file_name().unwrap().to_string_lossy()
        );
        for (name, text) in [
            ("claude", "#!/bin/sh\nprintf '[]\\n'\n"),
            (
                "tmux",
                "#!/bin/sh\nexec \"$TWIG_REAL_TMUX\" -L \"$TWIG_TEST_SOCKET\" -f /dev/null \"$@\"\n",
            ),
            (
                "codex",
                "#!/bin/sh\nif [ \"$TWIG_SLOW_CODEX\" = 1 ]; then sleep 30; fi\nprintf '%s\\n' '{\"status\":\"stopped\"}'\n",
            ),
        ] {
            let path = bin.join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let f = Self {
            registry: temp.path().join("registry.toml"),
            temp,
            repo,
            bin,
            socket,
            tmux,
        };
        f.git(&["init", "-b", "main"]);
        f.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        // 表单默认更新 base，所有网络操作使用本地隔离的远端。
        let remote = f.temp.path().join("base.git");
        f.git(&[
            "clone",
            "--bare",
            f.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ]);
        f.git(&["remote", "add", "base-source", remote.to_str().unwrap()]);
        f.git(&["fetch", "base-source"]);
        f.git(&["branch", "--set-upstream-to=base-source/main", "main"]);
        f
    }
    fn env(&self, c: &mut Command) {
        c.current_dir(self.temp.path())
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("TWIG_REAL_TMUX", &self.tmux)
            .env("TWIG_TEST_SOCKET", &self.socket)
            .env("TWIG_TEST_BIN", env!("CARGO_BIN_EXE_twig"))
            .env("TWIG_TEST_CONFIG", &self.registry)
            .env("TERM", "xterm-256color")
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
    }
    fn git(&self, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(&self.repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    fn twig(&self, args: &[&str]) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
        self.env(&mut c);
        c.current_dir(&self.repo)
            .arg("--config")
            .arg(&self.registry)
            .args(args);
        support::daemon::ensure(&c, &self.registry);
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    fn init(&self) {
        self.twig(&["add", "demo", "--path", self.repo.to_str().unwrap()]);
        self.twig(&["new", "demo", "auth", "--no-open"]);
    }
    fn remote_branch(&self, name: &str) -> Vec<u8> {
        self.git(&["checkout", "-b", name]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "remote work",
        ]);
        let commit = Command::new("git")
            .current_dir(&self.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        self.git(&["checkout", "main"]);
        let remote = self.temp.path().join("remote.git");
        self.git(&[
            "clone",
            "--bare",
            self.repo.to_str().unwrap(),
            remote.to_str().unwrap(),
        ]);
        self.git(&["branch", "-D", name]);
        for remote_name in ["origin", "upstream"] {
            self.git(&["remote", "add", remote_name, remote.to_str().unwrap()]);
            self.git(&["fetch", remote_name]);
            // 创建任务期间任何隐式联网都会失败。
            self.git(&[
                "remote",
                "set-url",
                remote_name,
                "/nonexistent/twig-test-remote",
            ]);
        }
        commit
    }
    fn terminal(&self, slow: bool) -> Pty {
        let mut command = Command::new("sh");
        self.env(&mut command);
        command.env("TWIG_SLOW_CODEX", if slow { "1" } else { "0" });
        command.args([
            "-c",
            r#"
"$TWIG_TEST_BIN" --config "$TWIG_TEST_CONFIG"
code=$?
printf '\nTERMINAL_RESTORED:%s\n' "$code"
"#,
        ]);
        Pty::spawn(command)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        support::daemon::stop_under(self.temp.path());
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}
struct ScreenSize {
    rows: usize,
    cols: usize,
}
impl alacritty_terminal::grid::Dimensions for ScreenSize {
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
struct Pty {
    child: Child,
    master: File,
    text: String,
    bytes: Vec<u8>,
    screen_bytes: Vec<u8>,
    original: libc::termios,
}
impl Pty {
    fn copies(&mut self, count: usize) -> Vec<String> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let started = Instant::now();
        let prefix = b"\x1b]52;c;";
        loop {
            self.read_output();
            let copies: Vec<_> = self
                .bytes
                .windows(prefix.len())
                .enumerate()
                .filter(|(_, bytes)| *bytes == prefix)
                .filter_map(|(start, _)| {
                    let start = start + prefix.len();
                    let end = start + self.bytes[start..].iter().position(|byte| *byte == 7)?;
                    Some(
                        String::from_utf8(STANDARD.decode(&self.bytes[start..end]).unwrap())
                            .unwrap(),
                    )
                })
                .collect();
            if copies.len() == count {
                return copies;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "复制超时：{}",
                self.text
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn spawn(mut command: Command) -> Self {
        support::daemon::for_pty(&command);
        let (mut master, mut slave) = (0, 0);
        let mut size = libc::winsize {
            ws_row: 36,
            ws_col: 140,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let original = terminal_attributes(master.as_raw_fd());
        let child = command.spawn().unwrap();
        assert_ne!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            -1
        );
        Self {
            child,
            master,
            text: String::new(),
            bytes: Vec::new(),
            screen_bytes: Vec::new(),
            original,
        }
    }
    fn created_task(&mut self, fixture: &Fixture, task: &str) {
        self.wait(" · 全局 Codex ");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&fixture.repo, &fixture.registry).join("state.json"))
                .unwrap(),
        )
        .unwrap();
        let path = state[task]["path"].as_str().unwrap();
        self.send(
            format!(
                "test \"$PWD\" = '{}' && printf 'CREATED_%s\\n' OK\r",
                path.replace('\'', "'\\''")
            )
            .as_bytes(),
        );
        self.wait("CREATED_OK");
        self.clear_output();
        self.send(b"\x17d");
        self.wait("任务 / ");
        self.wait(&format!("› {task}"));
    }
    fn send(&mut self, text: &[u8]) {
        self.master.write_all(text).unwrap();
    }
    fn clear_output(&mut self) {
        // 只清空当前阶段的捕获，保留屏幕记录以解析后续差量刷新。
        self.bytes.clear();
        self.text.clear();
    }
    fn read_output(&mut self) {
        let previous_len = self.bytes.len();
        let mut bytes = [0; 16384];
        // 每轮限制读取次数，让持续输出也不能阻止超时和退出检查。
        for _ in 0..16 {
            let mut ready = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut ready, 1, 0) };
            assert_ne!(
                result,
                -1,
                "PTY poll 失败：{}",
                std::io::Error::last_os_error()
            );
            if result == 0 || ready.revents & libc::POLLIN == 0 {
                break;
            }
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    self.bytes.extend_from_slice(&bytes[..n]);
                    self.screen_bytes.extend_from_slice(&bytes[..n]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                // Linux 在 PTY 的 slave 关闭后返回 EIO，macOS 返回 EOF。
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("读取 PTY 输出失败：{error}"),
            }
        }
        if self.bytes.len() != previous_len {
            self.text = strip_terminal_codes(&self.bytes);
        }
    }
    fn wait_exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            self.read_output();
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "等待 PTY 子进程 {} 退出超时，输出：{}",
                self.child.id(),
                self.text
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn wait(&mut self, needle: &str) {
        self.wait_for(needle, false, true);
    }
    fn task_form_ready(&mut self) {
        self.wait_screen("新建任务");
        // 等待正向就绪标志；PTY 分片到达时，“加载提示尚未绘制”不等于已加载。
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            self.read_output();
            let screen = self.screen_text();
            if screen.contains("已有分支 · 选作任务") || screen.contains("无匹配分支，将新建分支")
            {
                return;
            }
            assert!(Instant::now() < deadline, "分支候选未就绪：{screen}");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn wait_screen(&mut self, needle: &str) {
        self.wait_for(needle, true, true);
    }
    fn wait_screen_absent(&mut self, needle: &str) {
        self.wait_for(needle, true, false);
    }
    fn screen_text(&self) -> String {
        use alacritty_terminal::{
            Term,
            event::VoidListener,
            index::{Column, Line, Point},
            vte::ansi::Processor,
        };
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) },
            0
        );
        let size = ScreenSize {
            rows: size.ws_row.into(),
            cols: size.ws_col.into(),
        };
        let mut term = Term::new(Default::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, &self.screen_bytes);
        (0..size.rows)
            .map(|y| {
                (0..size.cols)
                    .filter_map(|x| {
                        let cell = &term.grid()[Point::new(Line(y as i32), Column(x))];
                        (!cell
                            .flags
                            .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER))
                        .then_some(cell.c)
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn wait_for(&mut self, needle: &str, screen: bool, present: bool) {
        let start = Instant::now();
        loop {
            self.read_output();
            let found = if screen {
                self.screen_text().contains(needle)
            } else {
                self.text.contains(needle)
            };
            if found == present {
                if needle.starts_with("TERMINAL_RESTORED:") {
                    let current = terminal_attributes(self.master.as_raw_fd());
                    assert_eq!(current.c_iflag, self.original.c_iflag);
                    assert_eq!(current.c_oflag, self.original.c_oflag);
                    assert_eq!(current.c_cflag, self.original.c_cflag);
                    // PENDIN 是内核重排待输入内容的瞬时标志，不属于 raw 模式设置。
                    assert_eq!(
                        current.c_lflag & !libc::PENDIN,
                        self.original.c_lflag & !libc::PENDIN
                    );
                    assert_eq!(current.c_cc, self.original.c_cc);
                }
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "等待 {needle} 可见={present} 超时：{}",
                if screen {
                    self.screen_text()
                } else {
                    self.text.clone()
                }
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe {
            libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size);
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

#[test]
fn pty_exit_drains_output_larger_than_the_terminal_buffer() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 65536 /dev/zero | tr '\\000' x; printf '\\nEXIT_DRAINED\\n'",
    ]);
    let mut p = Pty::spawn(command);
    assert!(p.wait_exit().success());
    p.wait("EXIT_DRAINED");
    assert!(p.bytes.len() >= 65536);
}

#[test]
fn branch_loading_keeps_input_responsive_and_blocks_early_submission() {
    let f = Fixture::new();
    f.init();
    f.git(&["branch", "feature/demo"]);
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let gate = f.temp.path().join("branch-query-gate");
    fs::write(&gate, "wait").unwrap();
    fs::write(f.bin.join("git"), "#!/bin/sh\nif [ \"$1\" = for-each-ref ]; then while [ -f \"$TWIG_BRANCH_GATE\" ]; do sleep 0.02; done; fi\nexec \"$TWIG_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(f.bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    command
        .env(
            "TWIG_REAL_GIT",
            String::from_utf8(real_git.stdout).unwrap().trim(),
        )
        .env("TWIG_BRANCH_GATE", &gate)
        .arg("--config")
        .arg(&f.registry);
    let mut p = Pty::spawn(command);
    p.wait("auth");
    p.send(b"2n");
    p.wait("正在读取分支候选");
    p.send(b"brand-new\r");
    p.wait("请稍候");
    assert!(
        !f.repo
            .with_file_name("repo.worktrees")
            .join("brand-new")
            .exists()
    );
    fs::remove_file(&gate).unwrap();
    p.send(&[127; 9]);
    p.wait_screen("feature/demo");
    p.send(b"\x03q");
    assert!(p.wait_exit().success());
}

#[test]
fn navigation_batches_worktree_and_window_queries() {
    let f = Fixture::new();
    f.init();
    f.twig(&["new", "demo", "second", "--no-open"]);
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let log = f.temp.path().join("queries");
    for (name, script) in [
        (
            "git",
            "#!/bin/sh\nprintf 'git %s\\n' \"$*\" >> \"$TWIG_QUERY_LOG\"\nexec \"$TWIG_REAL_GIT\" \"$@\"\n",
        ),
        (
            "tmux",
            "#!/bin/sh\nprintf 'tmux %s\\n' \"$*\" >> \"$TWIG_QUERY_LOG\"\nexec \"$TWIG_REAL_TMUX\" -L \"$TWIG_TEST_SOCKET\" -f /dev/null \"$@\"\n",
        ),
    ] {
        // 后台可能仍在执行旧脚本，不能原地截断可执行文件。
        let replacement = f.bin.join(format!("{name}.new"));
        fs::write(&replacement, script).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(replacement, f.bin.join(name)).unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    command
        .env(
            "TWIG_REAL_GIT",
            String::from_utf8(real_git.stdout).unwrap().trim(),
        )
        .env("TWIG_QUERY_LOG", &log)
        .arg("--config")
        .arg(&f.registry);
    let mut p = Pty::spawn(command);
    p.wait_screen("干净");
    let start = Instant::now();
    let queries = loop {
        let queries = fs::read_to_string(&log).unwrap();
        if queries
            .lines()
            .filter(|line| line.starts_with("git status "))
            .count()
            >= 3
        {
            break queries;
        }
        assert!(start.elapsed() < Duration::from_secs(3), "{queries}");
        thread::sleep(Duration::from_millis(10));
    };
    // 只检查第一轮三个任务的 Git 扫描，避免慢机器已进入后续扫描。
    let mut statuses = 0;
    let queries = queries
        .lines()
        .take_while(|line| {
            if statuses == 3 {
                return false;
            }
            statuses += usize::from(line.starts_with("git status "));
            true
        })
        .collect::<Vec<_>>();
    assert_eq!(
        queries
            .iter()
            .filter(|line| line.starts_with("git worktree list "))
            .count(),
        1
    );
    // 窗口采集有独立的两秒周期，不要求与一轮 Git 扫描次数相等；
    // 每次必须全局批量读取，不能按任务逐个查询窗口。
    let windows: Vec<_> = queries
        .iter()
        .filter(|line| line.starts_with("tmux list-windows "))
        .collect();
    assert!(!windows.is_empty());
    assert!(
        windows
            .iter()
            .all(|line| line.starts_with("tmux list-windows -a -F ")),
        "{windows:?}"
    );
    assert!(
        !queries
            .iter()
            .any(|line| line.contains("git branch --show-current"))
    );
    p.send(b"q");
    assert!(p.wait_exit().success());
}

#[test]
fn navigation_drag_copies_text_and_preserves_readonly_state() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let f = Fixture::new();
    f.init();
    let registry = fs::read(&f.registry).unwrap();
    let mut command = Command::new("sh");
    f.env(&mut command);
    command.env("SSH_CONNECTION", "twig-pty-test").args([
        "-c",
        "\"$TWIG_TEST_BIN\" --config \"$TWIG_TEST_CONFIG\"; printf '\\nTERMINAL_RESTORED:%s\\n' \"$?\"",
    ]);
    let mut p = Pty::spawn(command);
    p.wait("demo");
    p.send(b"\x1b[<0;4;6M\x1b[<32;7;6M\x1b[<0;7;6m");
    p.wait_screen("已请求复制 4 chars");
    let prefix = b"\x1b]52;c;";
    let start = p
        .bytes
        .windows(prefix.len())
        .position(|bytes| bytes == prefix)
        .unwrap()
        + prefix.len();
    let end = start + p.bytes[start..].iter().position(|byte| *byte == 7).unwrap();
    assert_eq!(STANDARD.decode(&p.bytes[start..end]).unwrap(), b"demo");
    p.wait_screen_absent("已请求复制");
    p.clear_output();
    p.send(b"\x1b[<0;5;6M\x1b[<0;5;6m\x1b[<0;5;6M\x1b[<0;5;6m\x1b[<0;5;6M\x1b[<0;5;6m");
    let started = Instant::now();
    let copies = loop {
        p.read_output();
        let copies: Vec<_> = p
            .bytes
            .windows(prefix.len())
            .enumerate()
            .filter(|(_, bytes)| *bytes == prefix)
            .filter_map(|(start, _)| {
                let start = start + prefix.len();
                let end = start + p.bytes[start..].iter().position(|byte| *byte == 7)?;
                Some(String::from_utf8(STANDARD.decode(&p.bytes[start..end]).unwrap()).unwrap())
            })
            .collect();
        if copies.len() == 2 {
            break copies;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "连击复制超时：{}",
            p.text
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(copies[0], "demo");
    assert!(
        copies[1].contains("demo") && copies[1].len() > copies[0].len(),
        "{copies:?}"
    );
    assert!(
        !copies[1].contains('│'),
        "选区不得包含边框或相邻区域：{copies:?}"
    );
    // 缩放清除旧坐标的选区；窄屏仍可拖选，Esc 只清除选区。
    p.resize(80, 24);
    p.clear_output();
    p.wait("demo");
    p.send(b"\x1b[<0;4;6M\x1b[<32;7;6M\x1b[<0;7;6m");
    p.wait_screen("已请求复制 4 chars");
    p.send(b"\x1bq");
    p.wait("TERMINAL_RESTORED:0");
    assert!(
        p.bytes
            .windows(b"\x1b[?1006l".len())
            .any(|bytes| bytes == b"\x1b[?1006l")
    );
    assert_eq!(fs::read(&f.registry).unwrap(), registry);
}

#[test]
fn monitor_and_footer_drag_and_triple_click_stay_in_their_regions() {
    let f = Fixture::new();
    f.init();
    let registry = fs::read(&f.registry).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    command
        .env("SSH_CONNECTION", "twig-pty-test")
        .arg("--config")
        .arg(&f.registry);
    let mut p = Pty::spawn(command);
    p.wait_screen("demo");
    for (width, height) in [(140, 36), (80, 24)] {
        p.resize(width, height);
        let expected = "Codex 服务未运行；Console 不自动启动服务".to_owned();
        let started = Instant::now();
        loop {
            p.read_output();
            if p.screen_text().lines().last().unwrap().trim_end() == expected {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "缩放后未显示底部状态：{}",
                p.text
            );
            thread::sleep(Duration::from_millis(10));
        }
        p.clear_output();
        // 拖动到上方内容区，仍只能选中底部状态这一行。
        p.send(
            format!(
                "\x1b[<0;1;{height}M\x1b[<32;{width};{}M\x1b[<0;{width};{}m",
                height - 2,
                height - 2
            )
            .as_bytes(),
        );
        assert_eq!(p.copies(1), vec![expected.clone()]);
        p.clear_output();
        let click = format!("\x1b[<0;2;{height}M\x1b[<0;2;{height}m");
        p.send(click.repeat(3).as_bytes());
        let copied = p.copies(2);
        assert_eq!(copied[1], expected);
        assert!(!copied[0].is_empty());
        // 全局监控在宽屏左下、窄屏下方，按真实画面定位。
        let monitor = "Codex 不可用，详见底部状态";
        p.wait_screen(monitor);
        let screen = p.screen_text();
        let (row, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(monitor))
            .unwrap();
        let col = line[..line.find(monitor).unwrap()].chars().count();
        p.clear_output();
        p.send(
            format!(
                "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
                col + 1,
                row + 1,
                col + 5,
                row + 1,
                col + 5,
                row + 1
            )
            .as_bytes(),
        );
        assert_eq!(p.copies(1), vec!["Codex"]);
        p.clear_output();
        let click = format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            col + 2,
            row + 1,
            col + 2,
            row + 1
        );
        p.send(click.repeat(3).as_bytes());
        assert_eq!(p.copies(2), vec!["Codex", monitor]);
    }
    p.send(b"q");
    assert!(p.wait_exit().success());
    assert_eq!(fs::read(&f.registry).unwrap(), registry);
}

#[test]
fn form_copy_stays_inside_modal_and_escape_clears_before_closing() {
    fn check_dialog(p: &mut Pty, title: &str, word: &str, width: u16, height: u16, deletion: bool) {
        p.wait_screen(title);
        let w = (width - 4).min(if deletion { 78 } else { 72 });
        let h = (height - 2).min(if deletion { 22 } else { 20 });
        let (left, top) = ((width - w) / 2 + 1, (height - h) / 2 + 1);
        let screen = p.screen_text();
        let (row, line) = screen
            .lines()
            .enumerate()
            .skip(top as usize)
            .take((h - 2) as usize)
            .find(|(_, line)| line.contains(word))
            .unwrap();
        let col = ratatui::text::Line::from(&line[..line.find(word).unwrap()]).width();
        p.clear_output();
        // 背景拖动不得产生复制，也不得计入弹窗连击。
        p.send(b"\x1b[<0;1;1M\x1b[<32;5;2M\x1b[<0;5;2m");
        let click = format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            col + 1,
            row + 1,
            col + 1,
            row + 1
        );
        p.send(click.repeat(3).as_bytes());
        let copied = p.copies(2);
        assert_eq!(copied[0], word);
        assert!(
            copied[1].contains(word) && !copied[1].contains('│'),
            "{copied:?}"
        );
        p.clear_output();
        p.send(
            format!(
                "\x1b[<0;{};{}M\x1b[<32;{width};{height}M\x1b[<0;{width};{height}m",
                left + 1,
                top + 1
            )
            .as_bytes(),
        );
        let copied = p.copies(1);
        for background in ["Console", "全局 Agent", title] {
            assert!(!copied[0].contains(background), "{copied:?}");
        }
        p.wait_screen("已请求复制");
        p.send(b"\x1b");
        p.wait_screen_absent("已请求复制");
        assert!(p.screen_text().contains(title), "首次 Esc 不能关闭弹窗");
    }
    let f = Fixture::new();
    f.init();
    f.git(&["branch", "existing"]);
    let registry = fs::read(&f.registry).unwrap();
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let state = fs::read(&state_path).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    command
        .env("SSH_CONNECTION", "twig-pty-test")
        .arg("--config")
        .arg(&f.registry);
    let mut p = Pty::spawn(command);
    p.wait_screen("demo");
    p.send(b"1n");
    check_dialog(&mut p, "登记已有仓库", "Git", 140, 36, false);
    p.send(b"\x1b");
    p.wait_screen_absent("登记已有仓库");
    p.resize(80, 24);
    p.send(b"2n");
    p.task_form_ready();
    check_dialog(&mut p, "新建任务", "demo", 80, 24, false);
    p.send(b"existing\r");
    check_dialog(&mut p, "使用已有分支", "existing", 80, 24, false);
    p.send(b"\x1b");
    p.wait_screen_absent("使用已有分支");
    p.send(b"\x1b");
    p.wait_screen_absent("新建任务");
    p.send(b"/auth\rdd");
    check_dialog(&mut p, "删除任务", "demo", 80, 24, true);
    p.send(b"\x1b");
    p.wait_screen_absent("删除任务");
    p.send(b"q");
    assert!(p.wait_exit().success());
    assert_eq!(fs::read(&f.registry).unwrap(), registry);
    assert_eq!(fs::read(state_path).unwrap(), state);
}

#[test]
fn noninteractive_console_is_explicit_and_completion_remains_readonly() {
    let f = Fixture::new();
    let mut old = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut old);
    let result = old.arg("dashboard").output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("unrecognized subcommand"));
    for args in [vec![], vec!["console"]] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut c);
        let out = c
            .arg("--config")
            .arg(&f.registry)
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("需要交互式终端"));
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut c);
    let out = c
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", "1")
        .args(["--", "twig", "cons"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("console"));
    assert!(!f.registry.exists());
}
#[test]
fn zed_links_follow_tasks_and_config_without_writing_registry() {
    let f = Fixture::new();
    f.init();
    let original = fs::read_to_string(&f.registry).unwrap();
    let configured = format!("[zed]\nssh_host = 'me@devbox:2222'\n{original}");
    fs::write(&f.registry, &configured).unwrap();
    let mut p = f.terminal(false);
    let wait_link = |p: &mut Pty, path: &std::path::Path| {
        let url = format!("\x1b]8;;zed://ssh/me@devbox:2222{}\x1b\\", path.display());
        let start = Instant::now();
        loop {
            p.read_output();
            if String::from_utf8_lossy(&p.bytes).contains(&url) {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(12),
                "PID {} 缺少链接 {url:?}：{}",
                p.child.id(),
                p.text
            );
            thread::sleep(Duration::from_millis(20));
        }
    };
    p.wait_screen("用 Zed 打开 ↗");
    p.send(b"2/main\r");
    wait_link(&mut p, &f.repo);
    p.clear_output();
    p.send(b"/auth\r");
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
    )
    .unwrap();
    wait_link(
        &mut p,
        std::path::Path::new(state["auth"]["path"].as_str().unwrap()),
    );
    assert_eq!(fs::read_to_string(&f.registry).unwrap(), configured);
    fs::write(&f.registry, &original).unwrap();
    p.send(b"r");
    p.wait_screen("[zed].ssh_host");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert_eq!(fs::read_to_string(&f.registry).unwrap(), original);
}

#[test]
fn empty_and_broken_registry_keep_terminal_usable_and_do_not_write() {
    let f = Fixture::new();
    let mut p = f.terminal(false);
    p.wait("尚未登记项目");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(!f.registry.exists());
    fs::write(&f.registry, "invalid {").unwrap();
    let mut p = f.terminal(false);
    p.wait("登记表读取失败");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert_eq!(fs::read_to_string(&f.registry).unwrap(), "invalid {");
}
#[test]
fn navigation_search_resize_and_quit_work_during_slow_queries() {
    let f = Fixture::new();
    f.init();
    let state = fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap();
    let mut p = f.terminal(true);
    p.wait("demo");
    p.send(b"2/auth\r");
    p.resize(65, 22);
    thread::sleep(Duration::from_millis(100));
    p.send(b"\x17j?");
    p.wait_screen("切换 Console 与 Agent");
    let start = Instant::now();
    p.send(b"\x03");
    p.wait("TERMINAL_RESTORED:0");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
        state
    );
    assert!(
        !support::project_dir(&f.repo, &f.registry)
            .join("lock")
            .exists()
    );
}
#[test]
fn task_visit_order_persists_and_browsing_is_readonly() {
    let f = Fixture::new();
    f.init();
    thread::sleep(Duration::from_millis(2));
    f.twig(&["new", "demo", "second", "--no-open"]);
    let created = fs::read(&f.registry).unwrap();
    let order = |p: &Pty, names: &[&str]| {
        let screen = p.screen_text();
        let positions: Vec<_> = names
            .iter()
            .map(|name| screen.find(name).unwrap())
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{screen}"
        );
    };
    let mut p = f.terminal(false);
    p.wait_screen("主工作目录");
    order(&p, &["second", "auth", "主工作目录"]);
    p.send(b"\r");
    p.wait_screen("› second");
    // 返回项目再进入时，清空任务筛选并重新选中排序第一项。
    p.send(b"/auth\r");
    p.wait_screen("任务 / auth");
    p.send(b"1\r");
    p.wait_screen("› second");
    assert_eq!(fs::read(&f.registry).unwrap(), created);
    p.send(b"2/main\r");
    p.wait_screen("任务 / main");
    assert_eq!(fs::read(&f.registry).unwrap(), created);
    p.send(b"\r");
    p.wait(" · 全局 Codex ");
    p.send(b"printf 'VISITED_%s\\n' MAIN\r");
    p.wait("VISITED_MAIN");
    p.send(b"\x17d");
    p.wait_screen("任务 / main");
    p.send(b"2/\r");
    p.wait_screen("second");
    // 等待返回导航后的最新快照；浏览不会刷新访问时间。
    let start = Instant::now();
    loop {
        p.wait_screen("主工作目录");
        let screen = p.screen_text();
        if screen.find("主工作目录") < screen.find("second") {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(50));
    }
    order(&p, &["主工作目录", "second", "auth"]);
    // 清空筛选和重排后，Enter 仍进入先前选中的主工作目录。
    p.send(b"\r");
    p.wait(" · 全局 Codex ");
    p.send(b"printf 'VISIT_DIR:%s\\n' \"$PWD\"\r");
    p.wait(&format!("VISIT_DIR:{}", f.repo.display()));
    p.send(b"\x17d");
    p.wait_screen("任务 / ");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    let visited = fs::read(&f.registry).unwrap();
    let mut p = f.terminal(false);
    p.wait_screen("主工作目录");
    order(&p, &["主工作目录", "second", "auth"]);
    // 主目录排第一时仍按列表顺序选中它。
    p.send(b"\r");
    p.wait_screen("› 主工作目录");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    assert_eq!(fs::read(&f.registry).unwrap(), visited);
}

#[test]
fn enter_main_and_task_releases_lock_and_restores_terminal() {
    let f = Fixture::new();
    f.init();
    for keys in [b"/main\r\r".as_slice(), b"/auth\r\r".as_slice()] {
        let mut p = f.terminal(false);
        p.wait("auth");
        let before_entry = fs::read(&f.registry).unwrap();
        p.send(b"\r");
        // 首次 Enter 仅进入任务区；搜索仍由 Console 处理。
        p.send(b"/auth");
        p.wait("搜索：");
        p.send(b"\r");
        // 返回项目后重新 Enter，应清除任务筛选，再按名称定位目标。
        p.send(b"1\r");
        assert_eq!(fs::read(&f.registry).unwrap(), before_entry);
        p.send(keys);
        p.wait(" · 全局 Codex ");
        p.send(b"printf 'ENTERED_%s\\n' SHELL\r");
        p.wait("ENTERED_SHELL");
        assert_ne!(fs::read(&f.registry).unwrap(), before_entry);
        let registry: toml::Value =
            toml::from_str(&fs::read_to_string(&f.registry).unwrap()).unwrap();
        assert!(
            registry["projects"]["demo"]["last_used"]
                .as_integer()
                .unwrap()
                > 0
        );
        p.send(b"printf 'SELECTED_DIR:%s\n' \"$PWD\"\r");
        let expected = if keys == b"/main\r\r" {
            f.repo.canonicalize().unwrap()
        } else {
            f.repo
                .with_file_name("repo.worktrees")
                .join("auth")
                .canonicalize()
                .unwrap()
        };
        p.wait(&format!("SELECTED_DIR:{}", expected.display()));

        assert!(
            !support::project_dir(&f.repo, &f.registry)
                .join("lock")
                .exists()
        );
        p.clear_output();
        p.send(b"\x17d");
        p.wait("任务 / ");
        // 返回后聚焦项目区，按 2 保留任务选择，可以再次进入同一 Shell。
        p.send(b"2\r");
        p.wait(" · 全局 Codex ");
        p.send(b"printf 'RETURNED_%s\\n' SHELL\r");
        p.wait("RETURNED_SHELL");
        p.clear_output();
        p.send(b"\x17d");
        p.wait("任务 / ");
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
        assert!(p.wait_exit().success());
    }
}

#[test]
fn workbench_split_resize_paste_disconnect_and_process_survival() {
    let f = Fixture::new();
    f.init();
    let mux = |args: &[&str]| {
        let out = Command::new(&f.tmux)
            .args(["-L", &f.socket])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2/auth\r\r");
    p.wait(" · 全局 Codex ");
    p.send(b"printf 'FIRST_%s\\n' PANE\r");
    p.wait("FIRST_PANE");
    let before = mux(&["list-panes", "-a", "-F", "#{pane_pid}"]);
    p.send(b"\x17s");
    p.send(b"printf 'SECOND_%s\\n' PANE\r");
    p.wait("SECOND_PANE");
    assert_eq!(mux(&["list-panes", "-a"]).lines().count(), 3);
    p.send(b"\x17z");
    p.resize(110, 30);
    p.send(b"printf 'ZOOM_%s\\n' OK\r");
    p.wait("ZOOM_OK");
    p.send(b"\x17z\x17o");
    p.send(b"\x1b[200~printf 'PASTE_%s\\n' OK\x1b[201~\r");
    p.wait("PASTE_OK");
    let pids = mux(&["list-panes", "-a", "-F", "#{pane_pid}"]);
    assert!(
        before
            .lines()
            .all(|pid| pids.lines().any(|after| after == pid))
    );
    p.clear_output();
    p.send(b"\x17d");
    p.wait("任务 / ");
    assert_eq!(pids, mux(&["list-panes", "-a", "-F", "#{pane_pid}"]));
    p.send(b"2\r");
    p.wait(" · 全局 Codex ");
    // 仅断开专用测试 server 的 Control Mode 客户端；Pane 仍在。
    let clients = mux(&["list-clients", "-F", "#{client_name}"]);
    for client in clients.lines() {
        mux(&["detach-client", "-t", client]);
    }
    p.clear_output();
    p.wait("任务 / ");
    assert_eq!(pids, mux(&["list-panes", "-a", "-F", "#{pane_pid}"]));
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn workbench_switches_projects_and_rejects_its_own_session() {
    let f = Fixture::new();
    f.init();
    let other = f.temp.path().join("other");
    assert!(
        Command::new("git")
            .arg("clone")
            .arg("--local")
            .arg(&f.repo)
            .arg(&other)
            .output()
            .unwrap()
            .status
            .success()
    );
    f.twig(&["add", "other", "--path", other.to_str().unwrap()]);
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"/other\r\r\r");
    p.wait(" · 全局 Codex ");
    p.send(b"printf 'PROJECT_DIR:%s\\n' \"$PWD\"\r");
    p.wait(&format!("PROJECT_DIR:{}", other.display()));
    p.send(b"\x17d");
    p.wait("任务 / ");
    p.clear_output();
    p.send(b"/demo\r\r/auth\r\r");
    p.wait(" · 全局 Codex ");
    p.send(b"printf 'TASK_DIR:%s\\n' \"$PWD\"\r");
    p.wait(&format!(
        "TASK_DIR:{}",
        f.repo
            .with_file_name("repo.worktrees")
            .join("auth")
            .display()
    ));
    p.send(b"\x17d");
    p.wait("任务 / ");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");

    let out = Command::new(&f.tmux)
        .args([
            "-L",
            &f.socket,
            "list-panes",
            "-a",
            "-F",
            "#{pane_id} #{pane_current_path}",
        ])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let pane = text
        .lines()
        .find(|line| line.ends_with(f.repo.to_str().unwrap()))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let mut command = Command::new("sh");
    f.env(&mut command);
    command.env("TMUX_PANE", pane).args([
        "-c",
        "\"$TWIG_TEST_BIN\" --config \"$TWIG_TEST_CONFIG\"; printf '\\nTERMINAL_RESTORED:%s\\n' $?",
    ]);
    let mut p = Pty::spawn(command);
    p.wait("auth");
    p.send(b"2/auth\r\r");
    p.wait("终端已返回导航：不能在当前");
    p.clear_output();
    p.send(b"2n");
    p.task_form_ready();
    p.send(b"feat/nested");
    p.wait_screen("无匹配分支，将新建分支");
    p.send(b"\r");
    p.wait("终端已返回导航：不能在当前");
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
    )
    .unwrap();
    assert!(PathBuf::from(state["feat/nested"]["path"].as_str().unwrap()).is_dir());
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn new_and_open_share_workbench_and_return_to_the_selected_task() {
    let f = Fixture::new();
    f.init();
    let target = f.repo.with_file_name("repo.worktrees").join("feat/unified");
    let mut previous_pids = None;
    for action in ["new", "open", "open"] {
        let mut command = Command::new("sh");
        f.env(&mut command);
        // 启动目录故意与目标任务无关，选中项必须来自显式命令参数。
        command.args(["-c", "\"$TWIG_TEST_BIN\" --config \"$TWIG_TEST_CONFIG\" \"$@\"; printf '\\nTERMINAL_RESTORED:%s\\n' $?",
            "test", action, "demo", "feat/unified"]);
        let mut p = Pty::spawn(command);
        // macOS 临时目录较长，详情栏需能完整显示路径，避免按换行片段匹配。
        p.resize(
            u16::try_from(target.as_os_str().len() * 2 + 20).unwrap(),
            36,
        );
        p.wait(" · 全局 Codex ");
        p.send(b"printf 'DIRECT_DIR:%s\\n' \"$PWD\"\r");
        p.wait(&format!("DIRECT_DIR:{}", target.display()));
        assert!(
            !support::project_dir(&f.repo, &f.registry)
                .join("lock")
                .exists()
        );
        let output = Command::new(&f.tmux)
            .args([
                "-L",
                &f.socket,
                "list-panes",
                "-a",
                "-F",
                "#{pane_pid} #{pane_current_path}",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let pids = String::from_utf8(output.stdout).unwrap();
        if let Some(previous) = &previous_pids {
            assert_eq!(previous, &pids);
        }
        previous_pids = Some(pids);
        p.clear_output();
        p.send(b"\x17d");
        p.wait("工作目录");
        p.wait(&target.display().to_string());
        p.send(b"i");
        p.wait("收起");
        p.clear_output();
        p.send(b"i");
        p.wait("更多");
        p.clear_output();
        p.send(b"2\r");
        p.wait(" · 全局 Codex ");
        p.send(b"printf 'REENTER_DIR:%s\\n' \"$PWD\"\r");
        p.wait(&format!("REENTER_DIR:{}", target.display()));
        p.clear_output();
        p.send(b"\x17d");
        p.wait("任务 / ");
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn custom_prefix_directions_alt_resize_and_mouse_work_in_terminal() {
    let f = Fixture::new();
    f.init();
    let mux = |args: &[&str]| {
        let out = Command::new(&f.tmux)
            .args(["-L", &f.socket])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2/auth\r\r");
    p.wait(" · 全局 Codex ");
    p.send(b"\x17s");
    p.send(b"printf 'SPLIT_%s\\n' READY\r");
    p.wait("SPLIT_READY");
    let rows = mux(&[
        "list-panes",
        "-a",
        "-F",
        "#{@twig-feature} #{pane_id} #{pane_left} #{pane_top} #{pane_width}",
    ]);
    let left = rows
        .lines()
        .filter(|row| row.starts_with("auth "))
        .map(|row| row.split_whitespace().collect::<Vec<_>>())
        .find(|row| row[2] == "0")
        .unwrap();
    let id = left[1];
    let y: u16 = left[3].parse().unwrap();
    let width: u16 = left[4].parse().unwrap();
    // Ctrl-W h 按方向切换；Alt-l 无前缀扩宽 3 列。
    p.send(b"\x17h\x1bl");
    p.send(b"printf 'KEYS_%s\\n' READY\r");
    p.wait("KEYS_READY");
    assert_eq!(
        mux(&["display-message", "-p", "-t", id, "#{pane_active}"]).trim(),
        "1"
    );
    assert_eq!(
        mux(&["display-message", "-p", "-t", id, "#{pane_width}"]).trim(),
        (width + 3).to_string()
    );
    p.send(b"\x17l");
    p.send(b"printf 'RIGHT_%s\\n' READY\r");
    p.wait("RIGHT_READY");
    // SGR 鼠标坐标从 1 开始；点击左 Pane 后，命令应在那里执行。
    p.send(format!("\x1b[<0;2;{}M\x1b[<0;2;{}m", y + 2, y + 2).as_bytes());
    p.send(b"printf 'CLICK_%s\\n' READY\r");
    p.wait("CLICK_READY");
    assert_eq!(
        mux(&["display-message", "-p", "-t", id, "#{pane_active}"]).trim(),
        "1"
    );
    let edge = width + 3;
    p.send(
        format!(
            "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<0;{};{}m",
            edge + 1,
            y + 2,
            edge + 4,
            y + 2,
            edge + 4,
            y + 2
        )
        .as_bytes(),
    );
    p.send(b"printf 'DRAG_%s\\n' READY\r");
    p.wait("DRAG_READY");
    assert_eq!(
        mux(&["display-message", "-p", "-t", id, "#{pane_width}"]).trim(),
        (width + 6).to_string()
    );
    p.clear_output();
    p.send(b"\x17d");
    p.wait("任务 / ");
    assert!(
        p.bytes
            .windows(b"\x1b[?1006l".len())
            .any(|bytes| bytes == b"\x1b[?1006l")
    );
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

fn strip_terminal_codes(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 27 && bytes.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < bytes.len() {
                let b = bytes[i];
                i += 1;
                if (0x40..=0x7e).contains(&b) {
                    break;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn codex_history_details_disconnect_and_reconnect_are_readonly() {
    use serde_json::{Value, json};
    use std::{
        os::unix::net::UnixListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    let f = Fixture::new();
    f.init();
    let sock = f.temp.path().join("codex.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' '{}'\n",
        json!({"status":"running","socketPath":sock})
            .to_string()
            .replace('\'', "'\\''")
    );
    fs::write(f.bin.join("codex"), script).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let offline = Arc::new(AtomicBool::new(false));
    let turn_failure = Arc::new(AtomicBool::new(false));
    let methods = Arc::new(Mutex::new(Vec::new()));
    let done = stop.clone();
    let disconnected = offline.clone();
    let failed_turn = turn_failure.clone();
    let seen = methods.clone();
    let cwd = f.repo.canonicalize().unwrap();
    let server = thread::spawn(move || {
        let mut connections = Vec::new();
        while !done.load(Ordering::Relaxed) {
            let (socket, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let done = done.clone();
            let disconnected = disconnected.clone();
            let failed_turn = failed_turn.clone();
            let seen = seen.clone();
            let cwd = cwd.clone();
            connections.push(thread::spawn(move || {
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = match tungstenite::accept(socket) {
                Ok(ws) => ws,
                Err(_) => return,
            };
            ws.get_mut()
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            while !done.load(Ordering::Relaxed) {
                let text = match ws.read() {
                    Ok(tungstenite::Message::Text(t)) => t,
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    _ => break,
                };
                let v: Value = serde_json::from_str(text.as_str()).unwrap();
                let method = v["method"].as_str().unwrap();
                seen.lock().unwrap().push(method.to_owned());
                if method == "initialized" {
                    continue;
                }
                if disconnected.load(Ordering::Relaxed) {
                    let _ = ws.close(None);
                    break;
                }
                if method == "thread/turns/list" && failed_turn.load(Ordering::Relaxed) {
                    if ws.send(tungstenite::Message::Text(json!({"id":v["id"],"error":{"code":-32603,"message":"TURN_QUERY_FAILURE"}}).to_string().into())).is_err() {
                        break;
                    }
                    continue;
                }
                let thread_value = |id: &str, status: Value| json!({"id":id,"cwd":cwd,"name":id,"status":status,"source":"cli","model":"mock-model"});
                let result = match method {
                    "initialize" => json!({}),
                    "thread/loaded/list" => json!({"data":["LIVE_THREAD"],"nextCursor":null}),
                    "thread/read" => {
                        json!({"thread":thread_value("LIVE_THREAD",json!({"type":"active","activeFlags":["waitingOnApproval"]}))})
                    }
                    "thread/list" => {
                        assert_eq!(v["params"]["useStateDbOnly"], true);
                        if v["params"]["cursor"].is_null() {
                            let mut temporary =
                                thread_value("TEMPORARY_HIDDEN", json!({"type":"idle"}));
                            temporary["ephemeral"] = json!(true);
                            json!({"data":[temporary,thread_value("HISTORY_ONE",json!({"type":"notLoaded"}))],"nextCursor":"two"})
                        } else {
                            json!({"data":[thread_value("HISTORY_TWO",json!({"type":"notLoaded"}))],"nextCursor":null})
                        }
                    }
                    "thread/turns/list" => {
                        json!({"data":[{"id":"turn-old","status":"completed","startedAt":1,"completedAt":2,"error":null}]})
                    }
                    other => panic!("不允许的请求：{other}"),
                };
                if ws
                    .send(tungstenite::Message::Text(
                        json!({"id":v["id"],"result":result}).to_string().into(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
            }));
        }
        for connection in connections {
            connection.join().unwrap();
        }
    });
    // 即使断言失败也释放模拟 server，不遗留线程。
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let guard = Stop(stop);
    let mut p = f.terminal(false);
    p.wait("LIVE_THR");
    p.send(b"2/main\r\x17jh");
    p.wait("HISTORY_");
    // 按协议日志等待实际分页完成，而非仅凭屏幕中历史提示。
    let start = Instant::now();
    while !methods.lock().unwrap().iter().any(|m| m == "thread/list") {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(20));
    }
    p.send(b"n");
    let start = Instant::now();
    while methods
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.as_str() == "thread/list")
        .count()
        < 2
    {
        assert!(start.elapsed() < Duration::from_secs(5), "历史下一页未完成");
        thread::sleep(Duration::from_millis(20));
    }
    p.send(b"\r");
    let start = Instant::now();
    while !methods
        .lock()
        .unwrap()
        .iter()
        .any(|m| m == "thread/turns/list")
    {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(20));
    }
    offline.store(true, Ordering::Relaxed);
    p.send(b"r");
    p.wait("服务关闭连接");
    p.wait("读取失败：");
    let prior_connections = methods
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.as_str() == "initialize")
        .count();
    p.clear_output();
    offline.store(false, Ordering::Relaxed);
    // 同一详情页不改查询键、不手动刷新，连接恢复后也应重试并结束错误状态。
    p.wait("本轮已完成");
    turn_failure.store(true, Ordering::Relaxed);
    p.clear_output();
    p.send(b"r");
    p.wait("TURN_QUERY_FAILURE");
    p.wait("读取失败：");
    p.clear_output();
    turn_failure.store(false, Ordering::Relaxed);
    // details RPC 成功但 snapshot.turn 为 Err，同样必须自动重试。
    p.wait("本轮已完成");
    let start = Instant::now();
    while methods
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.as_str() == "initialize")
        .count()
        <= prior_connections
    {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(20));
    }
    p.send(b"\x03");
    p.wait("TERMINAL_RESTORED:0");
    assert!(!p.text.contains("TEMPORAR"));
    drop(p);
    drop(guard);
    server.join().unwrap();
    let methods = methods.lock().unwrap();
    assert!(
        methods
            .iter()
            .filter(|m| m.as_str() == "thread/list")
            .count()
            >= 2
    );
}

#[test]
fn codex_completion_shortcut_from_workbench() {
    completion_shortcut(false, false, false);
}

#[test]
fn codex_completion_shortcut_from_navigation() {
    completion_shortcut(true, false, false);
}

#[test]
fn unread_picker_from_workbench() {
    completion_shortcut(false, true, false);
}

#[test]
fn unread_picker_from_navigation() {
    completion_shortcut(true, true, false);
}

#[test]
fn completed_turns_are_caught_up_after_restart() {
    completion_shortcut(false, false, true);
}

fn completion_shortcut(from_navigation: bool, use_picker: bool, restart: bool) {
    use serde_json::{Value, json};
    use std::{
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    let f = Fixture::new();
    f.init();
    let other = f.temp.path().join("other");
    assert!(
        Command::new("git")
            .args(["clone", "--local"])
            .arg(&f.repo)
            .arg(&other)
            .output()
            .unwrap()
            .status
            .success()
    );
    f.twig(&["add", "other", "--path", other.to_str().unwrap()]);
    let sock = f.temp.path().join("notify.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::write(
        f.bin.join("codex"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            json!({"status":"running","socketPath":sock})
                .to_string()
                .replace('\'', "'\\''")
        ),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let phase = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicUsize::new(usize::MAX));
    let done = stop.clone();
    let stage = phase.clone();
    let seen = observed.clone();
    let cwd = f
        .repo
        .with_file_name("repo.worktrees")
        .join("auth")
        .canonicalize()
        .unwrap();
    let server = thread::spawn(move || {
        let mut disconnected = false;
        while !done.load(Ordering::Relaxed) {
            let (socket, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(socket).unwrap();
            ws.get_mut()
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            while !done.load(Ordering::Relaxed) {
                let text = match ws.read() {
                    Ok(tungstenite::Message::Text(t)) => t,
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    _ => break,
                };
                let v: Value = serde_json::from_str(&text).unwrap();
                let current = stage.load(Ordering::Relaxed);
                let result = match v["method"].as_str().unwrap() {
                    "initialize" => json!({}),
                    "initialized" => continue,
                    "thread/loaded/list" if restart && current == 2 => json!({"data":[]}),
                    "thread/loaded/list" => json!({"data":["notify-thread"]}),
                    "thread/read" => {
                        json!({"thread":{"id":"notify-thread","cwd":cwd,"status":{"type":"idle"},"name":"NOTIFY_THREAD"}})
                    }
                    "thread/turns/list" => {
                        if current == 2 && !disconnected {
                            disconnected = true;
                            let _ = ws.close(None);
                            break;
                        }
                        seen.store(current, Ordering::Relaxed);
                        let mut turns = vec![json!({"id":"old","status":"completed"})];
                        if current > 0 {
                            turns.insert(0, json!({"id":"first","status":if current == 1 {"inProgress"} else {"completed"}}));
                        }
                        if current == 3 {
                            turns.insert(0, json!({"id":"second","status":"completed"}));
                        }
                        json!({"data":turns})
                    }
                    other => panic!("不允许的请求：{other}"),
                };
                if ws
                    .send(tungstenite::Message::Text(
                        json!({"id":v["id"],"result":result}).to_string().into(),
                    ))
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let guard = Stop(stop);
    let wait_phase = |expected| {
        let start = Instant::now();
        while observed.load(Ordering::Relaxed) != expected {
            assert!(start.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(20));
        }
    };
    let mut p = f.terminal(false);
    p.wait("auth");
    wait_phase(0);
    p.send(b"/other\r\r\r");
    p.wait(" · 全局 Codex ");
    assert!(!p.text.contains("本轮已完成"));
    if use_picker {
        p.send(b"\x17u");
        p.wait("暂无未读任务");
        p.send(b"jk\x17u");
        p.send(b"printf 'PICKER_CLOSED_%s\\n' OK\r");
        p.wait("PICKER_CLOSED_OK");
    }
    phase.store(1, Ordering::Relaxed);
    wait_phase(1);
    if restart {
        // 退出时仍在运行；完成发生在 Twig 不在线期间。
        p.send(b"\x17dq");
        p.wait("TERMINAL_RESTORED:0");
        assert!(p.wait_exit().success());
        phase.store(2, Ordering::Relaxed);
        p = f.terminal(false);
        p.wait("auth");
        p.send(b"/other\r\r\r");
        p.wait(" · 全局 Codex ");
    } else {
        phase.store(2, Ordering::Relaxed);
    }
    p.wait("Codex 本轮已完成");
    p.wait("Ctrl-W f 跳转");
    p.resize(80, 30);
    p.send(b"printf 'NOTIFY_INPUT_%s\\n' OK\r");
    p.wait("NOTIFY_INPUT_OK");
    p.resize(140, 36);
    if from_navigation {
        p.send(b"\x17d");
        p.wait("任务 / ");
    }
    p.clear_output();
    if use_picker {
        // 卡片到期后仍能选择未读；取消不会清除记录。
        thread::sleep(Duration::from_secs(9));
        p.send(b"\x17u");
        p.wait_screen("demo / auth / 未读 1");
        p.send(b"\x1b");
        p.wait_screen_absent("demo / auth / 未读 1");
        p.clear_output();
        p.send(b"\x17u");
        p.wait_screen("demo / auth / 未读 1");
        p.send(b"jk\x1b[B\x1b[A\r");
    } else {
        p.send(b"\x17f");
    }
    // 无提示时快捷键不退出终端，也不向 Shell 输入 f。
    p.send(b"\x17f");
    p.send(b"printf 'WINDOW_DIR:%s\\n' \"$PWD\"\r");
    p.wait(&format!(
        "WINDOW_DIR:{}",
        f.repo
            .with_file_name("repo.worktrees")
            .join("auth")
            .display()
    ));
    p.clear_output();
    p.resize(138, 36);
    p.send(b"printf 'DISMISSED_%s\\n' OK\r");
    p.wait("DISMISSED_OK");
    assert!(!p.text.contains("Codex 本轮已完成"), "{}", p.text);
    phase.store(3, Ordering::Relaxed);
    wait_phase(3);
    // 当前实际打开的是 auth：第二轮应静默，不排入下一张卡片。
    // 强制重绘，确认成功跳转后旧提示已关闭，静默轮次也没有提示。
    thread::sleep(Duration::from_secs(9));
    p.clear_output();
    p.resize(139, 36);
    p.send(b"printf 'SILENT_TURN_%s\\n' OK\r");
    p.wait("SILENT_TURN_OK");
    assert!(!p.text.contains("Codex 本轮已完成"), "{}", p.text);
    p.clear_output();
    p.send(b"\x17d");
    p.wait("任务 / ");
    assert!(!p.text.contains("[未读"), "{}", p.text);
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    drop(p);
    drop(guard);
    server.join().unwrap();
}

fn terminal_attributes(fd: std::os::fd::RawFd) -> libc::termios {
    let mut value = std::mem::MaybeUninit::uninit();
    assert_eq!(unsafe { libc::tcgetattr(fd, value.as_mut_ptr()) }, 0);
    unsafe { value.assume_init() }
}

#[test]
fn main_window_collision_is_reported_without_taking_over_or_closing_it() {
    let f = Fixture::new();
    f.init();
    let cfg: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("config.json")).unwrap(),
    )
    .unwrap();
    let target = format!("{}:main", cfg["session"].as_str().unwrap());
    let mux = |args: &[&str]| {
        let out = Command::new(&f.tmux)
            .args(["-L", &f.socket])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    mux(&["kill-window", "-t", &target]);
    let foreign = mux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        cfg["session"].as_str().unwrap(),
        "-n",
        "main",
        "-c",
        f.temp.path().to_str().unwrap(),
    ]);
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"\r/main\r\r");
    p.wait("无法确认归属");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert_eq!(
        mux(&["display-message", "-p", "-t", &target, "#{window_id}"]),
        foreign
    );
    assert!(
        !support::project_dir(&f.repo, &f.registry)
            .join("lock")
            .exists()
    );
}

#[test]
fn forms_register_create_retry_and_enter_task() {
    let mut f = Fixture::new();
    let spaced = f.temp.path().join("repo with spaces");
    fs::rename(&f.repo, &spaced).unwrap();
    f.repo = spaced;
    let mut p = f.terminal(false);
    p.wait("尚未登记项目");
    p.send(b"n");
    p.wait("登记已有仓库");
    p.clear_output();
    p.send(b"\x1b");
    // 弹窗外空列表未变化，检查被弹窗遮挡的详情区恢复。
    p.wait("任务详情");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(!f.registry.exists());
    assert!(!support::project_dir(&f.repo, &f.registry).exists());
    drop(p);

    let mut p = f.terminal(false);
    p.wait("尚未登记项目");
    p.send(b"/new\r");
    p.send(
        format!(
            "{}\t\x1b[Zdemo\r",
            f.repo.to_str().unwrap().trim_end_matches("th spaces")
        )
        .as_bytes(),
    );
    p.wait("操作成功");
    p.wait("主工作目录");
    assert!(f.registry.exists());
    assert!(
        support::project_dir(&f.repo, &f.registry)
            .join("config.json")
            .exists()
    );
    p.clear_output();
    // 创建项目后焦点已在任务区；通过候选补全打开任务表单。
    p.send(b"/ne\t\r");
    p.task_form_ready();
    let before_creation = fs::read(&f.registry).unwrap();
    p.send(b"feat/fresh\t\x7f\x7f\x7f\x7fmissing\r");
    p.wait("基础分支不存在");
    assert_eq!(fs::read(&f.registry).unwrap(), before_creation);
    assert!(
        !support::project_dir(&f.repo, &f.registry)
            .join("state.json")
            .exists()
    );
    p.clear_output();
    p.send(b"\x7f\x7f\x7f\x7f\x7f\x7f\x7fmain\r");
    p.created_task(&f, "feat/fresh");
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["feat/fresh"]["base"], "main");
    let registry: toml::Value = toml::from_str(&fs::read_to_string(&f.registry).unwrap()).unwrap();
    assert!(
        registry["projects"]["demo"]["last_used"]
            .as_integer()
            .unwrap()
            > 0
    );
    assert!(PathBuf::from(state["feat/fresh"]["path"].as_str().unwrap()).is_dir());
    assert!(
        !support::project_dir(&f.repo, &f.registry)
            .join("lock")
            .exists()
    );
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn console_rejects_duplicates_and_rechecks_registration_after_confirmation() {
    for during_confirmation in [false, true] {
        let f = Fixture::new();
        f.init();
        if during_confirmation {
            f.git(&["branch", "pending"]);
        }
        let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        if during_confirmation {
            p.send(b"pending\r");
            p.wait("使用已有分支");
            let mut state: serde_json::Value =
                serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            state["pending"] = serde_json::json!({"branch": "pending", "base": "main", "path": f.repo.with_file_name("repo.worktrees").join("pending")});
            fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let before = fs::read(&state_path).unwrap();
        let registry = fs::read(&f.registry).unwrap();
        p.send(if during_confirmation {
            b"\t\r"
        } else {
            b"auth\r"
        });
        p.wait("已存在，请从任务列表进入");
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert_eq!(fs::read(&f.registry).unwrap(), registry);
        assert!(
            !f.repo
                .with_file_name("repo.worktrees")
                .join("pending")
                .exists()
        );
        p.send(b"\x03q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn task_form_creates_typed_prefix_and_clears_selection_after_editing() {
    for select_then_edit in [false, true] {
        let f = Fixture::new();
        f.init();
        f.git(&["branch", "optimize"]);
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        p.send(b"op");
        p.wait_screen("optimize");
        p.wait_screen("Enter 按输入创建");
        if select_then_edit {
            p.send(b"\x1b[B");
            p.wait_screen("Enter 选中并确认");
        }
        p.send(b"t");
        p.wait_screen("Enter 按输入创建");
        p.send(b"\r");
        p.created_task(&f, "opt");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["opt"]["branch"], "opt");
        assert!(state.get("optimize").is_none());
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn task_form_selects_existing_branch_without_creating_another_branch() {
    let f = Fixture::new();
    f.init();
    f.git(&["branch", "feature/alpha"]);
    f.git(&["branch", "feature/beta"]);
    f.git(&["update-ref", "refs/remotes/origin/feature/remote", "HEAD"]);
    let refs = || {
        Command::new("git")
            .current_dir(&f.repo)
            .args([
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/",
            ])
            .output()
            .unwrap()
            .stdout
    };
    let before = refs();
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let state_before = fs::read(&state_path).unwrap();
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2n");
    p.task_form_ready();
    p.send(b"feature/");
    p.wait("feature/alpha");
    p.wait("feature/beta");
    assert!(!p.text.contains("feature/remote"));
    p.send(b"\x1b[B\x1b[B\t\r");
    p.wait("使用已有分支");
    // 默认取消，候选选择本身不创建资源。
    p.send(b"\r");
    p.clear_output();
    p.task_form_ready();
    assert_eq!(fs::read(&state_path).unwrap(), state_before);
    assert_eq!(refs(), before);
    p.send(b"\r");
    p.wait("使用已有分支");
    p.send(b"\t\r");
    p.created_task(&f, "feature/beta");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(state["feature/beta"]["branch"], "feature/beta");
    assert!(state.get("feature/alpha").is_none());
    assert!(PathBuf::from(state["feature/beta"]["path"].as_str().unwrap()).is_dir());
    assert_eq!(refs(), before);
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn identical_candidate_labels_preserve_the_selected_git_reference() {
    for accept in [b"\r".as_slice(), b"\t\r".as_slice()] {
        let f = Fixture::new();
        f.init();
        let commit = f.remote_branch("feat/collision");
        f.git(&["branch", "upstream/feat/collision", "main"]);
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        p.send(b"upstream/feat/collision");
        p.wait("upstream/feat/collision");
        // 本地与远端候选显示同名，第二项才是远端跟踪分支。
        p.send(b"\x1b[B\x1b[B");
        p.send(accept);
        p.wait("使用已有分支");
        // 默认取消后再次提交，仍须保持同一远端候选。
        p.send(b"\r");
        p.task_form_ready();
        p.clear_output();
        p.send(b"\r");
        p.wait("使用已有分支");
        p.send(b"\t\r");
        p.created_task(&f, "feat/collision");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
        )
        .unwrap();
        assert!(state.get("upstream/feat/collision").is_none());
        assert_eq!(
            state["feat/collision"]["remote_start"]["reference"],
            "refs/remotes/upstream/feat/collision"
        );
        let output = Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "feat/collision"])
            .output()
            .unwrap();
        assert_eq!(output.stdout, commit);
        let output = Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-list", "--count", "main..upstream/feat/collision"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "0");
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn remote_candidates_fill_task_names_and_preserve_local_precedence() {
    for local in [false, true] {
        let f = Fixture::new();
        f.init();
        let remote_commit = f.remote_branch("feat/remote");
        f.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);
        if local {
            f.git(&["branch", "feat/remote", "main"]);
        }
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        p.send(b"upstream/feat");
        p.wait("upstream/feat/remote");
        assert!(!p.text.contains("origin/HEAD"));
        p.send(b"\t\r");
        p.wait("使用已有分支");
        if !local {
            // Tab 填入后仍保留所选的 upstream，不回到 origin。
            p.wait("upstream/feat/remote");
        }
        p.send(b"\t\r");
        p.created_task(&f, "feat/remote");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(state["feat/remote"]["branch"], "feat/remote");
        assert!(state.get("upstream/feat/remote").is_none());
        let git_output = |args: &[&str]| {
            Command::new("git")
                .current_dir(&f.repo)
                .args(args)
                .output()
                .unwrap()
        };
        if local {
            assert!(state["feat/remote"]["remote_start"].is_null());
            assert_eq!(
                git_output(&["rev-parse", "feat/remote"]).stdout,
                git_output(&["rev-parse", "main"]).stdout
            );
        } else {
            assert_eq!(
                state["feat/remote"]["remote_start"]["reference"],
                "refs/remotes/upstream/feat/remote"
            );
            assert_eq!(
                git_output(&["rev-parse", "feat/remote"]).stdout,
                remote_commit
            );
            assert_eq!(
                String::from_utf8(
                    git_output(&["rev-parse", "--abbrev-ref", "feat/remote@{upstream}"]).stdout
                )
                .unwrap()
                .trim(),
                "upstream/feat/remote"
            );
        }
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn enter_uses_highlighted_remote_and_rejects_registered_tasks() {
    for stale in [false, true] {
        let f = Fixture::new();
        f.init();
        let commit = f.remote_branch("feat/km_story");
        // 两个远端指向不同提交，确保最终选中的确实是 upstream。
        f.git(&["update-ref", "refs/remotes/origin/feat/km_story", "main"]);
        let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
        if stale {
            let mut state: serde_json::Value =
                serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            state["feat/km_story"] = serde_json::json!({
                "branch": "feat/km_story", "base": "main",
                "path": f.repo.with_file_name("repo.worktrees").join("feat/km_story"),
            });
            fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
            f.env(&mut command);
            let output = command
                .arg("--config")
                .arg(&f.registry)
                .args(["new", "demo", "feat/km_story", "--no-open"])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("已存在"));
        }
        let before = fs::read(&state_path).unwrap();
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        p.send(b"km");
        p.wait("upstream/feat/km_story");
        p.clear_output();
        // 不按 Tab，主动选择第二项后直接使用高亮候选。
        p.send(b"\x1b[B\x1b[B\r");
        if stale {
            p.wait("已存在，请从任务列表进入");
            assert_eq!(fs::read(&state_path).unwrap(), before);
            assert!(
                !f.repo
                    .with_file_name("repo.worktrees")
                    .join("feat/km_story")
                    .exists()
            );
            p.send(b"\x03q");
            p.wait("TERMINAL_RESTORED:0");
            continue;
        }
        p.wait("分支 upstream/feat/km_story 已存在");
        assert!(!p.text.contains("选择来源"));
        p.send(b"\r");
        p.clear_output();
        p.task_form_ready();
        assert_eq!(fs::read(&state_path).unwrap(), before);
        p.send(b"\r");
        p.wait("分支 upstream/feat/km_story 已存在");
        // 已锁定来源，方向键和 j/k 不切换来源，也不重置确认按钮。
        p.send(b"\tj\x1b[A\x1b[Bk\r");
        p.created_task(&f, "feat/km_story");
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert!(state.get("km").is_none());
        assert_eq!(
            state["feat/km_story"]["remote_start"]["reference"],
            "refs/remotes/upstream/feat/km_story"
        );
        let output = Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "feat/km_story"])
            .output()
            .unwrap();
        assert_eq!(output.stdout, commit);
        assert!(
            !Command::new("git")
                .current_dir(&f.repo)
                .args(["show-ref", "--verify", "refs/heads/km"])
                .output()
                .unwrap()
                .status
                .success()
        );
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn selected_source_cannot_disappear_or_replace_a_recorded_source_silently() {
    for recorded in [false, true] {
        let f = Fixture::new();
        f.init();
        let commit = f.remote_branch("feat/km_story");
        let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
        if recorded {
            let mut state: serde_json::Value =
                serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
            state["feat/km_story"] = serde_json::json!({
                "branch": "feat/km_story", "base": "main",
                "path": f.repo.with_file_name("repo.worktrees").join("feat/km_story"),
                "remote_start": { "reference": "refs/remotes/origin/feat/km_story",
                    "commit": String::from_utf8(commit).unwrap().trim() },
            });
            fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let before = fs::read(&state_path).unwrap();
        let mut p = f.terminal(false);
        p.wait("auth");
        p.send(b"2n");
        p.task_form_ready();
        p.send(b"upstream/feat");
        p.wait("upstream/feat/km_story");
        if !recorded {
            f.git(&["update-ref", "-d", "refs/remotes/upstream/feat/km_story"]);
        }
        p.send(b"\x1b[B\r");
        p.wait(if recorded {
            "已存在，请从任务列表进入"
        } else {
            "所选分支已不存在"
        });
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(
            !Command::new("git")
                .current_dir(&f.repo)
                .args(["show-ref", "--verify", "refs/heads/feat/km_story"])
                .output()
                .unwrap()
                .status
                .success()
        );
        p.send(b"\x03");
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
    }
}

#[test]
fn remote_candidate_handles_remote_names_with_slashes() {
    let f = Fixture::new();
    f.init();
    f.git(&[
        "remote",
        "add",
        "team/mirror",
        "/nonexistent/twig-test-remote",
    ]);
    f.git(&["update-ref", "refs/remotes/team/mirror/fix/login", "HEAD"]);
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2n");
    p.task_form_ready();
    p.send(b"team/mirror/");
    p.wait("team/mirror/fix/login");
    p.send(b"\t\r");
    p.wait("使用已有分支");
    p.send(b"\t\r");
    p.created_task(&f, "fix/login");
    let output = Command::new("git")
        .current_dir(&f.repo)
        .args(["config", "branch.fix/login.remote"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "team/mirror"
    );
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn existing_branch_cli_requires_confirmation_and_preserves_commit() {
    let f = Fixture::new();
    f.init();
    f.git(&["checkout", "-b", "existing"]);
    f.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "existing work",
    ]);
    let original = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    f.git(&["checkout", "main"]);
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let before = fs::read(&state_path).unwrap();
    for answer in ["\n", "y\n"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut command);
        command.arg("--config").arg(&f.registry).args([
            "new",
            "demo",
            "existing",
            "--base",
            "missing",
            "--no-open",
        ]);
        let mut p = Pty::spawn(command);
        p.wait("[y/N]");
        assert_eq!(fs::read(&state_path).unwrap(), before);
        p.send(answer.as_bytes());
        let status = p.wait_exit();
        assert_eq!(status.success(), answer == "y\n");
    }
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    let path = PathBuf::from(state["existing"]["path"].as_str().unwrap());
    assert!(path.is_dir());
    let output = Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(output.stdout, original);
}

#[test]
fn existing_branch_console_defaults_to_cancel_and_checks_occupancy() {
    let f = Fixture::new();
    f.init();
    f.git(&["branch", "existing"]);
    let mut p = f.terminal(false);
    p.wait("主工作目录");
    p.send(b"2/new\r");
    p.task_form_ready();
    p.send(b"existing\r");
    p.wait("使用已有分支");
    p.send(b"\r");
    p.clear_output();
    p.send(b"\r");
    p.wait("使用已有分支");
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let before = fs::read(&state_path).unwrap();
    assert!(!String::from_utf8_lossy(&before).contains("existing"));
    let occupied = f.temp.path().join("occupied");
    f.git(&["worktree", "add", occupied.to_str().unwrap(), "existing"]);
    p.send(b"\t\r");
    // 居中弹窗内错误可能换行，分别检查原因和占用路径。
    p.wait("已被其他 Worktree");
    p.wait("occupied");
    assert_eq!(fs::read(&state_path).unwrap(), before);
    f.git(&["worktree", "remove", occupied.to_str().unwrap()]);
    p.clear_output();
    p.send(b"\r");
    p.wait("使用已有分支");
    p.send(b"\t\r");
    p.created_task(&f, "existing");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    assert_eq!(state["existing"]["branch"], "existing");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn remote_branch_cli_selects_source_and_rejects_changed_snapshot() {
    let f = Fixture::new();
    f.init();
    let commit = f.remote_branch("feat/remote");
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let before = fs::read(&state_path).unwrap();
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut command);
        command.arg("--config").arg(&f.registry).args([
            "new",
            "demo",
            "feat/remote",
            "--base",
            "missing",
            "--no-open",
        ]);
        command
    };
    let output = command().output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("交互终端"));
    let mut p = Pty::spawn(command());
    p.wait("分支编号：");
    p.send(b"\n");
    assert!(!p.wait_exit().success());
    assert_eq!(fs::read(&state_path).unwrap(), before);
    let mut p = Pty::spawn(command());
    p.wait("分支编号：");
    p.send(b"2\n");
    p.wait("[y/N]");
    f.git(&["update-ref", "refs/remotes/upstream/feat/remote", "main"]);
    p.send(b"y\n");
    p.wait("资源已变化");
    assert!(!p.wait_exit().success());
    assert_eq!(fs::read(&state_path).unwrap(), before);
    f.git(&[
        "update-ref",
        "refs/remotes/upstream/feat/remote",
        std::str::from_utf8(&commit).unwrap().trim(),
    ]);
    let mut p = Pty::spawn(command());
    p.wait("分支编号：");
    p.send(b"2\n");
    p.wait("[y/N]");
    p.send(b"y\n");
    assert!(p.wait_exit().success());
    let head = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "feat/remote"])
        .output()
        .unwrap();
    assert_eq!(head.stdout, commit);
    let tracking = Command::new("git")
        .current_dir(&f.repo)
        .args([
            "rev-parse",
            "--symbolic-full-name",
            "feat/remote@{upstream}",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(tracking.stdout).unwrap().trim(),
        "refs/remotes/upstream/feat/remote"
    );
    let output = command().output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("已存在"));
}

#[test]
fn remote_branch_single_source_and_local_precedence() {
    for local in [false, true] {
        let f = Fixture::new();
        f.init();
        let remote_commit = f.remote_branch("feat/remote");
        f.git(&["remote", "remove", "origin"]);
        let expected = if local {
            f.git(&["branch", "feat/remote", "main"]);
            Command::new("git")
                .current_dir(&f.repo)
                .args(["rev-parse", "main"])
                .output()
                .unwrap()
                .stdout
        } else {
            remote_commit
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut command);
        command
            .arg("--config")
            .arg(&f.registry)
            .args(["new", "demo", "feat/remote", "--no-open"]);
        let mut p = Pty::spawn(command);
        p.wait("[y/N]");
        assert!(!p.text.contains("分支编号"));
        assert_eq!(p.text.contains("upstream/feat/remote"), !local);
        p.send(b"y\n");
        assert!(p.wait_exit().success());
        let output = Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "feat/remote"])
            .output()
            .unwrap();
        assert_eq!(output.stdout, expected);
    }
}

#[test]
fn remote_branch_console_selects_source_and_rejects_partial_creation_retry() {
    let f = Fixture::new();
    f.init();
    let commit = f.remote_branch("feat/remote");
    // Git 已创建分支/Worktree 后 hook 失败；重试不得从 main 重建。
    let hook = f.repo.join(".git/hooks/post-checkout");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let mut p = f.terminal(false);
    p.wait("主工作目录");
    p.send(b"2/new\r");
    p.task_form_ready();
    // 手动输入后切到起点字段提交，不接受候选，仍允许选择来源。
    p.send(b"feat/remote\x1b[Z\r");
    p.wait("origin/feat/remote");
    p.send(b"\t");
    // 切换来源重置为取消，Enter 返回表单。
    p.send(b"j\r");
    p.clear_output();
    p.send(b"\r");
    p.wait("origin/feat/remote");
    p.send(b"j");
    p.wait("upstream/feat/remote");
    p.send(b"\t\r");
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    // 创建失败保留记录及资源，再次新建必须拒绝。
    p.wait("执行失败");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(
        state["feat/remote"]["remote_start"]["reference"],
        "refs/remotes/upstream/feat/remote"
    );
    fs::remove_file(hook).unwrap();
    p.send(b"\r");
    p.wait("已存在，请从任务列表进入");
    let head = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "feat/remote"])
        .output()
        .unwrap();
    assert_eq!(head.stdout, commit);
    p.send(b"\x03q");
    p.wait("TERMINAL_RESTORED:0");
}

#[test]
fn register_initialized_repository_reuses_alias_and_rejects_conflict() {
    let f = Fixture::new();
    f.init();
    let config = fs::read(support::project_dir(&f.repo, &f.registry).join("config.json")).unwrap();
    let run = |name: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut command);
        command
            .arg("--config")
            .arg(&f.registry)
            .args(["add", name, "--print-name", "--path"])
            .arg(&f.repo)
            .output()
            .unwrap()
    };
    let before = fs::read(&f.registry).unwrap();
    let result = run("another");
    assert!(result.status.success(), "{:?}", result);
    assert!(String::from_utf8_lossy(&result.stdout).ends_with("\"demo\"\n"));
    assert_eq!(before, fs::read(&f.registry).unwrap());
    // 已初始化但未登记时补登记，不改项目配置。
    fs::remove_file(&f.registry).unwrap();
    assert!(run("again").status.success());
    assert_eq!(
        config,
        fs::read(support::project_dir(&f.repo, &f.registry).join("config.json")).unwrap()
    );
    let other = Fixture::new();
    let registry = fs::read_to_string(&f.registry).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    let result = command
        .arg("--config")
        .arg(&f.registry)
        .args(["add", "again", "--print-name", "--path"])
        .arg(&other.repo)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(registry, fs::read_to_string(&f.registry).unwrap());
    assert!(
        !support::project_dir(&other.repo, &other.registry)
            .join("config.json")
            .exists()
    );
}

#[test]
fn focus_shortcuts_switch_lists_without_triggering_history_or_forms() {
    let f = Fixture::new();
    f.init();
    let mut p = f.terminal(false);
    p.wait("auth");
    p.resize(65, 22);
    for (keys, expected) in [
        (b"\x17l".as_slice(), "任务 /"),
        (b"\x17j".as_slice(), "Codex 已加载"),
        (b"\x17k".as_slice(), "任务 /"),
        (b"\x17h".as_slice(), "› demo"),
        (b"2".as_slice(), "任务 /"),
        (b"1".as_slice(), "› demo"),
    ] {
        p.clear_output();
        p.send(keys);
        p.wait(expected);
    }
    // 数字快捷键保持任务筛选；搜索中的数字仍是查询内容。
    p.send(b"2/auth12\r");
    // 切走再切回以读取完整标题，避免把终端差量更新当成完整文本。
    for (keys, expected) in [(b"1", "› demo"), (b"2", "任务 / auth12")] {
        p.clear_output();
        p.send(keys);
        p.wait(expected);
    }
    // 表单中的数字仍可输入，取消不创建任务。
    p.send(b"n");
    p.task_form_ready();
    p.send(b"task12");
    p.wait("2▏");
    p.resize(66, 22);
    p.wait("task12");
    p.send(b"\x03");
    p.wait_screen_absent("新建任务");
    p.send(b"1");
    p.wait_screen("项目 /");
    // 前缀取消后 n 仍打开项目表单；表单内 Ctrl-W 不改变焦点。
    p.send(b"\x17");
    p.clear_output();
    p.send(b"\x1b");
    // 等待独立 Esc 被解析，避免与后续 n 合并为 Alt-n。
    thread::sleep(Duration::from_millis(200));
    p.send(b"n");
    p.wait_screen("登记已有仓库");
    p.send(b"\x17l");
    p.send(b"\x03");
    p.send(b"\x03");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn project_path_enter_validates_then_prefills_alias_without_writing() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join("src")).unwrap();
    let mut p = f.terminal(false);
    p.wait("尚未登记项目");
    p.send(b"n\r");
    p.wait("请输入仓库路径");
    assert!(!p.text.contains("已创建的资源会保留"));
    p.send(format!("{}/missing\r", f.temp.path().display()).as_bytes());
    p.wait("目录不存在");
    assert!(!f.registry.exists());
    p.send(&vec![
        127;
        format!("{}/missing", f.temp.path().display())
            .chars()
            .count()
    ]);
    p.send(format!("{}/\r", f.repo.display()).as_bytes());
    p.wait("Enter 登记");
    assert!(!f.registry.exists());
    assert!(!support::project_dir(&f.repo, &f.registry).exists());
    // 使用默认 repo 别名直接提交；完整目录不误选子目录 src。
    p.send(b"\r");
    p.wait("操作成功");
    let registry = fs::read_to_string(&f.registry).unwrap();
    assert!(registry.contains("[projects.repo]"));
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn delete_task_confirmation_protection_and_keep_branch() {
    let f = Fixture::new();
    f.init();
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let path = PathBuf::from(state["auth"]["path"].as_str().unwrap());
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"\r/main\r/delete\r");
    p.wait("不能删除主工作目录");
    p.send(b"/auth\r/delete\r");
    p.wait("确认删除");
    // 默认取消，Enter 不产生写操作。
    p.send(b"\r");
    p.wait("主工作目录");
    assert!(path.is_dir());
    fs::write(path.join("unsaved"), "keep me").unwrap();
    p.send(b"/delete\r\t\r");
    p.wait("删除失败");
    assert!(path.is_dir());
    assert!(fs::read_to_string(&state_path).unwrap().contains("auth"));
    fs::remove_file(path.join("unsaved")).unwrap();
    // 失败后回到取消，取消勾选删除分支，再明确选择确认。
    p.clear_output();
    p.send(b" \t\r");
    p.wait_screen("已选中 demo / main");
    assert!(!path.exists());
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(state.get("auth").is_none());
    f.git(&["show-ref", "--verify", "refs/heads/auth"]);
    p.send(b"/delete\r");
    p.wait("不能删除主工作目录");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn delete_task_removes_branch_and_rejects_changed_identity() {
    let f = Fixture::new();
    f.init();
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let path = PathBuf::from(state["auth"]["path"].as_str().unwrap());
    // 未合并且领先本地 upstream 的提交，Console 显式删除应允许清理。
    f.git(&["branch", "--set-upstream-to=main", "auth"]);
    f.git(&[
        "-C",
        path.to_str().unwrap(),
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "unmerged and unpushed",
    ]);
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"\r/auth\rd");
    assert!(PathBuf::from(state["auth"]["path"].as_str().unwrap()).is_dir());
    p.send(b"d");
    p.wait("确认删除");
    // 默认勾选删除本地分支，确认后执行。
    state["auth"]["branch"] = "changed".into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    p.send(b"\t\r");
    p.wait("任务归属已变化");
    assert!(PathBuf::from(state["auth"]["path"].as_str().unwrap()).is_dir());
    state["auth"]["branch"] = "auth".into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    p.clear_output();
    fs::write(path.join("unsaved"), "keep me").unwrap();
    p.send(b"\t\r");
    p.wait("未提交、未跟踪");
    assert!(path.is_dir());
    f.git(&["show-ref", "--verify", "refs/heads/auth"]);
    fs::remove_file(path.join("unsaved")).unwrap();
    p.clear_output();
    p.send(b"\t\r");
    p.wait_screen("已选中 demo / main");
    let output = Command::new("git")
        .current_dir(&f.repo)
        .args(["show-ref", "--verify", "refs/heads/auth"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn delete_ignored_defaults_on_and_keeps_default_cancel() {
    let f = Fixture::new();
    f.init();
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
    )
    .unwrap();
    let path = PathBuf::from(state["auth"]["path"].as_str().unwrap());
    fs::write(f.repo.join(".git/info/exclude"), ".venv/\n").unwrap();
    fs::create_dir(path.join(".venv")).unwrap();
    fs::write(path.join(".venv/data"), "ignored").unwrap();
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"\r/auth\rdd");
    p.wait("同时删除被忽略文件");
    p.send(b"\x1b[6~");
    p.wait(".venv/");
    // 默认勾选 ignored 删除；关闭后仍保护 ignored 文件。
    p.send(b"j \t\r");
    p.wait("删除失败");
    assert!(path.join(".venv/data").exists());
    // 切换选项会回到默认取消，Enter 不删除。
    p.send(b"\x1b[B \r");
    p.wait("主工作目录");
    assert!(path.join(".venv/data").exists());
    p.clear_output();
    p.send(b"dd\t\r");
    p.wait_screen("已选中 demo / main");
    assert!(!path.exists());
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn delivered_agent_messages_notify_without_daemon_and_jump_to_task() {
    let mut f = Fixture::new();
    f.registry = f.temp.path().join("config.toml");
    f.init();
    support::daemon::stop_under(f.temp.path());
    let _ = fs::remove_file(f.temp.path().join("notifications.json"));
    let project_state = support::project_dir(&f.repo, &f.registry).join("state.json");
    let mut tasks: serde_json::Value =
        serde_json::from_slice(&fs::read(&project_state).unwrap()).unwrap();
    let task_path = tasks["auth"]["path"].clone();
    tasks["auth"]["agent_thread"] = serde_json::json!("task-thread");
    fs::write(&project_state, serde_json::to_vec(&tasks).unwrap()).unwrap();
    let dir = f.temp.path().join("controller");
    fs::create_dir(&dir).unwrap();
    let mut state = serde_json::json!({
        "bindings": {
            "controller": {"thread":"controller-thread", "cwd":dir, "project":null, "task":null, "stopped":false},
            "task-thread": {"thread":"task-thread", "cwd":task_path, "project":"demo", "task":"auth", "stopped":false}
        },
        "messages": [{"id":"old", "thread":"task-thread", "text":"历史", "status":"sent", "turn":null, "error":null}],
        "seen":[], "socket":null
    });
    let save = |state: &serde_json::Value| {
        let temp = dir.join("next.json");
        fs::write(&temp, serde_json::to_vec(state).unwrap()).unwrap();
        fs::rename(temp, dir.join("runtime.json")).unwrap();
    };
    save(&state);
    let mut p = f.terminal(false);
    p.wait("auth");
    thread::sleep(Duration::from_secs(3));
    p.send(b"\x17u");
    p.wait("暂无未读任务");
    p.send(b"\x17u");
    state["messages"].as_array_mut().unwrap().push(serde_json::json!({"id":"new", "thread":"task-thread", "text":"新派活", "status":"sent", "turn":null, "error":null}));
    fs::write(&project_state, b"temporarily incomplete JSON").unwrap();
    save(&state);
    p.wait("未读检查失败");
    fs::write(&project_state, serde_json::to_vec(&tasks).unwrap()).unwrap();
    p.wait("Agent 收到新消息");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    p = f.terminal(false);
    p.wait("auth");
    p.send(b"\x17u");
    p.wait_screen("demo / auth / 未读 1");
    // 重启恢复未读，但不重播旧卡片；通过未读列表进入。
    p.send(b"\r");
    p.wait(" · 全局 Codex ");
    p.send(b"\x17u");
    p.wait("暂无未读任务");
    p.send(b"\x17u\x17d");
    p.wait("任务 / ");
    state["messages"].as_array_mut().unwrap().push(serde_json::json!({"id":"feedback", "thread":"controller-thread", "text":"子任务反馈", "status":"sent", "turn":null, "error":null}));
    save(&state);
    p.wait("主控有新消息");
    p.wait("Agent*");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    state["messages"].as_array_mut().unwrap().push(serde_json::json!({"id":"offline", "thread":"task-thread", "text":"退出期间送达", "status":"sent", "turn":null, "error":null}));
    save(&state);
    p = f.terminal(false);
    p.wait("Agent*");
    p.wait("Agent 收到新消息");
    p.send(b"\x17u");
    p.wait_screen("demo / auth / 未读 1");
    p.send(b"\x17u");
    p.wait_screen_absent("demo / auth / 未读 1");
    p.send(b"\t");
    p.wait("YOLO");
    p.send(b"\t");
    p.wait_screen_absent("Agent*");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(f.temp.path().join("notifications.json")).unwrap())
            .unwrap();
    assert_eq!(saved["unread"].as_array().unwrap().len(), 1);
    assert_eq!(saved["unread"][0]["id"], "message:offline");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(dir.join("runtime.json")).unwrap())
            .unwrap(),
        state
    );
}

#[test]
fn claude_state_changes_notify_and_jump_without_hooks_or_agent_control() {
    let mut f = Fixture::new();
    f.registry = f.temp.path().join("config.toml");
    f.init();
    let tasks: serde_json::Value = serde_json::from_slice(
        &fs::read(support::project_dir(&f.repo, &f.registry).join("state.json")).unwrap(),
    )
    .unwrap();
    let path = tasks["auth"]["path"].clone();
    let save = |status: &str| {
        let json = serde_json::json!([{"cwd":path, "kind":"interactive", "startedAt":1,
            "sessionId":"claude-live", "name":"claude-live", "status":status}]);
        fs::write(
            f.bin.join("claude.next"),
            serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
        fs::rename(f.bin.join("claude.next"), f.bin.join("claude.json")).unwrap();
    };
    save("busy");
    fs::write(f.bin.join("claude"), "#!/bin/sh\n[ \"$*\" = 'agents --json --all' ] || exit 9\nexec /bin/cat \"${0%/*}/claude.json\"\n").unwrap();
    let mut p = f.terminal(false);
    p.wait("claude-live");
    p.send(b"\x17u");
    p.wait("暂无未读任务");
    p.send(b"\x17u");
    save("idle");
    p.wait("Claude 状态变化");
    p.send(b"\x17u");
    p.wait_screen("demo / auth / 未读 1");
    p.send(b"\x17u\x17f");
    p.wait(" · 全局 Codex ");
    p.send(b"\x17u");
    p.wait_screen("暂无未读任务");
    p.send(b"\x17u\x17d");
    p.wait("任务 / ");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
    assert!(!f.repo.join(".claude/settings.json").exists());
    assert!(!f.temp.path().join("controller/workspace").exists());
}

#[test]
fn new_form_fetch_failure_can_retry_with_checkbox_disabled() {
    let f = Fixture::new();
    f.init();
    f.git(&["branch", "--unset-upstream", "main"]);
    let state_path = support::project_dir(&f.repo, &f.registry).join("state.json");
    let before = fs::read(&state_path).unwrap();
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2n");
    p.wait_screen("[x] 更新远端 base");
    p.send(b"fresh-offline\r");
    p.wait("未配置远端 upstream");
    assert_eq!(fs::read(&state_path).unwrap(), before);
    p.send(b"\x1b[Z ");
    p.wait_screen("[ ] 更新远端 base");
    p.send(b"\r");
    p.created_task(&f, "fresh-offline");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn new_form_uses_latest_remote_base_by_default() {
    let f = Fixture::new();
    f.init();
    f.git(&["checkout", "-b", "publish"]);
    f.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-m",
        "remote update",
    ]);
    f.git(&["push", "base-source", "publish:main"]);
    let remote_commit = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    f.git(&["checkout", "main"]);
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2n");
    p.wait_screen("[x] 更新远端 base");
    p.send(b"fresh-remote\r");
    p.created_task(&f, "fresh-remote");
    let commit = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "fresh-remote"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(commit, remote_commit);
    let main = Command::new("git")
        .current_dir(&f.repo)
        .args(["rev-parse", "main"])
        .output()
        .unwrap()
        .stdout;
    assert_ne!(main, remote_commit);
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.wait_exit().success());
}

#[test]
fn task_creation_enters_workbench_while_startup_writes_files() {
    let f = Fixture::new();
    f.init();
    fs::create_dir_all(f.repo.join(".twig")).unwrap();
    fs::write(
        f.repo.join(".twig/config.toml"),
        r#"[tmux]
panes = [{ command = ["sh", "-c", "i=0; while [ $i -lt 100 ]; do touch startup-$i; i=$((i+1)); sleep 0.02; done; echo STARTUP_DONE"] }]
"#,
    ).unwrap();
    let mut p = f.terminal(false);
    p.wait("auth");
    p.send(b"2n");
    p.task_form_ready();
    p.send(b"writer\r");
    p.wait(" · 全局 Codex ");
    p.wait_screen("STARTUP_DONE");
    p.created_task(&f, "writer");
    assert!(
        f.repo
            .with_file_name("repo.worktrees")
            .join("writer/startup-99")
            .is_file()
    );
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
}
