use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
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
        let temp = tempfile::tempdir().unwrap();
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
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    fn init(&self) {
        self.twig(&["init", "--name", "demo"]);
        self.twig(&["new", "auth", "--no-open"]);
    }
    fn terminal(&self, slow: bool, repo: Option<&str>) -> Pty {
        let mut command = Command::new("sh");
        self.env(&mut command);
        command.env("TWIG_SLOW_CODEX", if slow { "1" } else { "0" });
        command.args(["-c",r#"
if [ -n "$TWIG_SELECTED" ]; then "$TWIG_TEST_BIN" --config "$TWIG_TEST_CONFIG" --repo "$TWIG_SELECTED"; else "$TWIG_TEST_BIN" --config "$TWIG_TEST_CONFIG"; fi
code=$?
printf '\nTERMINAL_RESTORED:%s\n' "$code"
"#]);
        if let Some(name) = repo {
            command.env("TWIG_SELECTED", name);
        } else {
            command.env_remove("TWIG_SELECTED");
        }
        Pty::spawn(command)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}
struct Pty {
    child: Child,
    master: File,
    text: String,
    bytes: Vec<u8>,
    original: libc::termios,
}
impl Pty {
    fn spawn(mut command: Command) -> Self {
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
                    &mut size,
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
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
        }
        Self {
            child,
            master,
            text: String::new(),
            bytes: Vec::new(),
            original,
        }
    }
    fn send(&mut self, text: &[u8]) {
        self.master.write_all(text).unwrap();
    }
    fn wait(&mut self, needle: &str) {
        let start = Instant::now();
        loop {
            let mut bytes = [0; 16384];
            while let Ok(n) = self.master.read(&mut bytes) {
                if n == 0 {
                    break;
                }
                self.bytes.extend_from_slice(&bytes[..n]);
                self.text = strip_terminal_codes(&self.bytes);
            }
            if self.text.contains(needle) {
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
                "未看到 {needle}: {}",
                self.text
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
fn empty_and_broken_registry_keep_terminal_usable_and_do_not_write() {
    let f = Fixture::new();
    let mut p = f.terminal(false, None);
    p.wait("尚未登记项目");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(!f.registry.exists());
    fs::write(&f.registry, "invalid {").unwrap();
    let mut p = f.terminal(false, None);
    p.wait("登记表读取失败");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert_eq!(fs::read_to_string(&f.registry).unwrap(), "invalid {");
}
#[test]
fn navigation_search_resize_and_quit_work_during_slow_queries() {
    let f = Fixture::new();
    f.init();
    let state = fs::read(f.repo.join(".git/twig/state.json")).unwrap();
    let mut p = f.terminal(true, Some("demo"));
    p.wait("demo");
    p.send(b"\t/auth\r");
    p.resize(65, 22);
    thread::sleep(Duration::from_millis(100));
    p.send(b"\t?");
    p.wait("Shift+Tab");
    let start = Instant::now();
    p.send(b"\x03");
    p.wait("TERMINAL_RESTORED:0");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fs::read(f.repo.join(".git/twig/state.json")).unwrap(),
        state
    );
    assert!(!f.repo.join(".git/twig/lock").exists());
}
#[test]
fn enter_main_and_task_releases_lock_and_restores_terminal() {
    let f = Fixture::new();
    f.init();
    for keys in [b"Ggg\r".as_slice(), b"G\r".as_slice()] {
        let mut p = f.terminal(false, Some("demo"));
        p.wait("auth");
        p.send(b"\r");
        // 首次 Enter 仅进入任务区；搜索仍由 Console 处理。
        p.send(b"/auth");
        p.wait("搜索：");
        p.send(b"\r");
        // 返回项目后重新 Enter，应清除任务筛选并选中主工作目录。
        p.send(b"\x1b[Z\r");
        p.send(keys);
        p.wait("[twig-");
        p.send(b"printf 'ENTERED_%s\\n' SHELL\r");
        p.wait("ENTERED_SHELL");
        p.send(b"printf 'SELECTED_DIR:%s\n' \"$PWD\"\r");
        let expected = if keys == b"Ggg\r" {
            f.repo.canonicalize().unwrap()
        } else {
            f.repo
                .with_file_name("repo.worktrees")
                .join("auth")
                .canonicalize()
                .unwrap()
        };
        p.wait(&format!("SELECTED_DIR:{}", expected.display()));

        assert!(!f.repo.join(".git/twig/lock").exists());
        p.bytes.clear();
        p.text.clear();
        p.send(b"\x02d");
        p.wait("任务 / ");
        // 返回后保留任务区焦点和选择，可以再次进入同一 Shell。
        p.send(b"\r");
        p.wait("[twig-");
        p.send(b"printf 'RETURNED_%s\\n' SHELL\r");
        p.wait("RETURNED_SHELL");
        p.bytes.clear();
        p.text.clear();
        p.send(b"\x02d");
        p.wait("任务 / ");
        p.send(b"q");
        p.wait("TERMINAL_RESTORED:0");
        assert!(p.child.wait().unwrap().success());
    }
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
    let methods = Arc::new(Mutex::new(Vec::new()));
    let done = stop.clone();
    let disconnected = offline.clone();
    let seen = methods.clone();
    let cwd = f.repo.canonicalize().unwrap();
    let server = thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            let (socket, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            socket
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let mut ws = match tungstenite::accept(socket) {
                Ok(ws) => ws,
                Err(_) => continue,
            };
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
                        json!({"data":[{"status":"completed","startedAt":1,"completedAt":2,"error":null}]})
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
    // 即使断言失败也释放模拟 server，不遗留线程。
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let guard = Stop(stop);
    let mut p = f.terminal(false, Some("demo"));
    p.wait("LIVE_THR");
    p.send(b"h");
    p.wait("HISTORY_");
    // 按协议日志等待实际分页完成，而非仅凭屏幕中历史提示。
    let start = Instant::now();
    while !methods.lock().unwrap().iter().any(|m| m == "thread/list") {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(20));
    }
    p.send(b"n");
    thread::sleep(Duration::from_millis(200));
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
    offline.store(false, Ordering::Relaxed);
    p.send(b"r");
    let start = Instant::now();
    while methods
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.as_str() == "initialize")
        .count()
        < 2
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

fn terminal_attributes(fd: std::os::fd::RawFd) -> libc::termios {
    let mut value = std::mem::MaybeUninit::uninit();
    assert_eq!(unsafe { libc::tcgetattr(fd, value.as_mut_ptr()) }, 0);
    unsafe { value.assume_init() }
}

#[test]
fn main_window_collision_is_reported_without_taking_over_or_closing_it() {
    let f = Fixture::new();
    f.init();
    let cfg: serde_json::Value =
        serde_json::from_slice(&fs::read(f.repo.join(".git/twig/config.json")).unwrap()).unwrap();
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
    let mut p = f.terminal(false, Some("demo"));
    p.wait("auth");
    p.send(b"\r\r");
    p.wait("无法确认归属");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert_eq!(
        mux(&["display-message", "-p", "-t", &target, "#{window_id}"]),
        foreign
    );
    assert!(!f.repo.join(".git/twig/lock").exists());
}

#[test]
fn forms_register_create_retry_and_cancel_without_entering_shell() {
    let mut f = Fixture::new();
    let spaced = f.temp.path().join("repo with spaces");
    fs::rename(&f.repo, &spaced).unwrap();
    f.repo = spaced;
    let mut p = f.terminal(false, None);
    p.wait("尚未登记项目");
    p.send(b"n");
    p.wait("登记已有仓库");
    p.bytes.clear();
    p.text.clear();
    p.send(b"\x1b");
    p.wait("尚未登记项目");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(!f.registry.exists());
    assert!(!f.repo.join(".git/twig").exists());
    drop(p);

    let mut p = f.terminal(false, None);
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
    assert!(f.repo.join(".git/twig/config.json").exists());
    p.bytes.clear();
    p.text.clear();
    // 创建项目后焦点已在任务区；通过候选补全打开任务表单。
    p.send(b"/ne\t\r");
    p.wait("新建任务");
    p.send(b"feat/fresh\t\x7f\x7f\x7f\x7fmissing\r");
    p.wait("基础分支不存在");
    assert!(!f.repo.join(".git/twig/state.json").exists());
    p.send(b"\x7f\x7f\x7f\x7f\x7f\x7f\x7fmain\r");
    p.wait("操作成功");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(f.repo.join(".git/twig/state.json")).unwrap()).unwrap();
    assert_eq!(state["feat/fresh"]["base"], "main");
    assert!(PathBuf::from(state["feat/fresh"]["path"].as_str().unwrap()).is_dir());
    assert!(!f.repo.join(".git/twig/lock").exists());
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.child.wait().unwrap().success());
}

#[test]
fn register_initialized_repository_reuses_alias_and_rejects_conflict() {
    let f = Fixture::new();
    f.init();
    let config = fs::read(f.repo.join(".git/twig/config.json")).unwrap();
    let run = |name: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        f.env(&mut command);
        command
            .arg("--config")
            .arg(&f.registry)
            .args(["__console-register", "--name", name, "--path"])
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
        fs::read(f.repo.join(".git/twig/config.json")).unwrap()
    );
    let other = Fixture::new();
    let registry = fs::read_to_string(&f.registry).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    f.env(&mut command);
    let result = command
        .arg("--config")
        .arg(&f.registry)
        .args(["__console-register", "--name", "again", "--path"])
        .arg(&other.repo)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(registry, fs::read_to_string(&f.registry).unwrap());
    assert!(!other.repo.join(".git/twig/config.json").exists());
}

#[test]
fn ctrl_w_directions_switch_lists_without_triggering_history_or_forms() {
    let f = Fixture::new();
    f.init();
    let mut p = f.terminal(false, Some("demo"));
    p.wait("auth");
    p.resize(65, 22);
    for (keys, expected) in [
        (b"\x17l".as_slice(), "任务 /"),
        (b"\x17j".as_slice(), "Codex 已加载"),
        (b"\x17k".as_slice(), "任务 /"),
        (b"\x17h".as_slice(), "› demo"),
    ] {
        p.bytes.clear();
        p.text.clear();
        p.send(keys);
        p.wait(expected);
    }
    // 前缀取消后 n 仍打开项目表单；表单内 Ctrl-W 不改变焦点。
    p.send(b"\x17");
    p.wait("Ctrl-W：");
    p.bytes.clear();
    p.text.clear();
    p.send(b"\x1b");
    p.wait("Tab / Ctrl-W");
    p.send(b"n");
    p.wait("登记已有仓库");
    p.send(b"\x17l");
    p.send(b"\x03");
    p.send(b"\x03");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.child.wait().unwrap().success());
}

#[test]
fn project_path_enter_validates_then_prefills_alias_without_writing() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join("src")).unwrap();
    let mut p = f.terminal(false, None);
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
    p.wait("可修改别名");
    assert!(!f.registry.exists());
    assert!(!f.repo.join(".git/twig").exists());
    // 使用默认 repo 别名直接提交；完整目录不误选子目录 src。
    p.send(b"\r");
    p.wait("操作成功");
    let registry = fs::read_to_string(&f.registry).unwrap();
    assert!(registry.contains("[projects.repo]"));
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.child.wait().unwrap().success());
}

#[test]
fn delete_task_confirmation_protection_and_keep_branch() {
    let f = Fixture::new();
    f.init();
    let state_path = f.repo.join(".git/twig/state.json");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let path = PathBuf::from(state["auth"]["path"].as_str().unwrap());
    let mut p = f.terminal(false, Some("demo"));
    p.wait("auth");
    p.send(b"\r/delete\r");
    p.wait("不能删除主工作目录");
    p.send(b"G/delete\r");
    p.wait("确认删除");
    // 默认取消，Enter 不产生写操作。
    p.send(b"\r");
    p.wait("主工作目录");
    assert!(path.is_dir());
    fs::write(path.join("unsaved"), "keep me").unwrap();
    p.send(b"/delete\r\t\r");
    p.wait("未提交");
    assert!(path.is_dir());
    assert!(fs::read_to_string(&state_path).unwrap().contains("auth"));
    fs::remove_file(path.join("unsaved")).unwrap();
    // 失败后回到取消，空格保留分支，再明确选择确认。
    p.bytes.clear();
    p.text.clear();
    p.send(b" \t\r");
    p.wait("已选中 demo / main");
    assert!(!path.exists());
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    assert!(state.get("auth").is_none());
    f.git(&["show-ref", "--verify", "refs/heads/auth"]);
    p.send(b"/delete\r");
    p.wait("不能删除主工作目录");
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.child.wait().unwrap().success());
}

#[test]
fn delete_task_removes_branch_and_rejects_changed_identity() {
    let f = Fixture::new();
    f.init();
    let state_path = f.repo.join(".git/twig/state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let mut p = f.terminal(false, Some("demo"));
    p.wait("auth");
    p.send(b"\rGd");
    p.wait("再按 d 打开删除确认");
    assert!(PathBuf::from(state["auth"]["path"].as_str().unwrap()).is_dir());
    p.send(b"d");
    p.wait("确认删除");
    state["auth"]["branch"] = "changed".into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    p.send(b"\t\r");
    p.wait("任务归属已变化");
    assert!(PathBuf::from(state["auth"]["path"].as_str().unwrap()).is_dir());
    state["auth"]["branch"] = "auth".into();
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    p.bytes.clear();
    p.text.clear();
    p.send(b"\t\r");
    p.wait("已选中 demo / main");
    let output = Command::new("git")
        .current_dir(&f.repo)
        .args(["show-ref", "--verify", "refs/heads/auth"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    p.send(b"q");
    p.wait("TERMINAL_RESTORED:0");
    assert!(p.child.wait().unwrap().success());
}
