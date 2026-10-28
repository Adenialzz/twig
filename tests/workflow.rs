mod support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    tmux: String,
    socket: String,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo with ' spaces");
        let root = temp.path().join("work trees");
        let bin = temp.path().join("bin");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&bin).unwrap();
        let output = Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .unwrap();
        assert!(output.status.success(), "集成测试需要安装 tmux");
        let tmux = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        let socket = format!(
            "twig-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let wrapper = bin.join("tmux");
        fs::write(&wrapper, format!("#!/bin/sh\nif [ \"$TWIG_FAIL_TMUX\" = 1 ] && [ \"$1\" = new-window ]; then echo injected-failure >&2; exit 71; fi\nexec '{}' -L '{}' -f /dev/null \"$@\"\n", tmux.replace('\'', "'\\''"), socket)).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            bin.join("codex"),
            "#!/bin/sh\nprintf '%s\\n' '{\"status\":\"stopped\"}'\n",
        )
        .unwrap();
        fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self {
            _temp: temp,
            repo,
            root,
            bin,
            tmux,
            socket,
        };
        f.git(&["init", "-b", "main"]);
        f.git(&["config", "user.email", "test@example.invalid"]);
        f.git(&["config", "user.name", "Twig Test"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f.git(&["commit", "--allow-empty", "-m", "initial"]);
        f.ok(&[
            "add",
            "local",
            "--path",
            f.repo.to_str().unwrap(),
            "--root",
            f.root.to_str().unwrap(),
        ]);
        f
    }
    fn command(&self, cwd: &Path) -> Command {
        self.command_with_registry(cwd, &self.registry_path())
    }
    fn command_with_registry(&self, cwd: &Path, registry: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
        c.arg("--config").arg(registry);
        c.current_dir(cwd)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        support::daemon::ensure(&c, registry);
        c
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(&self.repo).args(args).output().unwrap()
    }
    fn registry_path(&self) -> PathBuf {
        self._temp.path().join("user-config/config.toml")
    }
    fn registered(&self, args: &[&str]) -> Output {
        self.command(self._temp.path()).args(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        success(self.run(args))
    }
    fn fail(&self, args: &[&str], expected: &str) {
        let out = self.run(args);
        assert!(!out.status.success(), "应拒绝：{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn git(&self, args: &[&str]) -> String {
        success(
            Command::new("git")
                .current_dir(&self.repo)
                .args(args)
                .output()
                .unwrap(),
        )
    }
    fn mux(&self, args: &[&str]) -> String {
        success(
            Command::new(&self.tmux)
                .args(["-L", &self.socket])
                .args(args)
                .output()
                .unwrap(),
        )
    }
    fn session(&self) -> String {
        let v: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&self.repo, &self.registry_path()).join("config.json"))
                .unwrap(),
        )
        .unwrap();
        v["session"].as_str().unwrap().to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        support::daemon::stop_under(self._temp.path());
        let _ = Command::new(&self.tmux)
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn startup_mixed_modes_keep_process_output_and_close_shell() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".twig")).unwrap();
    let literal = "quote ' $(touch unwanted) ;";
    let layout = serde_json::json!({"tmux": {"panes": [
        {"mode": "shell", "focus": true},
        {"split": "right", "mode": "process", "command": ["/bin/echo", literal]},
        {"split": "down", "mode": "process", "command": ["/usr/bin/false"]},
        {"split": "down", "mode": "process", "command": ["/bin/sleep", "60"]}
    ]}});
    fs::write(
        f.repo.join(".twig/config.toml"),
        toml::to_string(&layout).unwrap(),
    )
    .unwrap();
    f.ok(&["new", "local", "--no-fetch", "mixed", "--no-open"]);
    let target = format!("{}:mixed", f.session());
    let output = f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"]);
    let panes: Vec<_> = output.lines().collect();
    assert_eq!(panes.len(), 4);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while f
        .mux(&[
            "display-message",
            "-p",
            "-t",
            panes[3],
            "#{pane_current_command}",
        ])
        .trim()
        != "sleep"
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f.mux(&["send-keys", "-t", panes[3], "C-c"]);
    for pane in &panes[1..] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while f
            .mux(&["display-message", "-p", "-t", pane, "#{pane_dead}"])
            .trim()
            != "1"
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    assert_eq!(
        f.mux(&[
            "display-message",
            "-p",
            "-t",
            panes[1],
            "#{pane_dead_status}"
        ])
        .trim(),
        "0"
    );
    assert_eq!(
        f.mux(&[
            "display-message",
            "-p",
            "-t",
            panes[2],
            "#{pane_dead_status}"
        ])
        .trim(),
        "1"
    );
    assert!(
        f.mux(&["capture-pane", "-p", "-S", "-", "-t", panes[1]])
            .contains(literal)
    );
    assert!(!f.root.join("mixed/unwanted").exists());
    f.mux(&[
        "send-keys",
        "-t",
        panes[0],
        "-l",
        "printf ready > shell-ready",
    ]);
    f.mux(&["send-keys", "-t", panes[0], "Enter"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while fs::read_to_string(f.root.join("mixed/shell-ready"))
        .ok()
        .as_deref()
        != Some("ready")
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f.mux(&["send-keys", "-t", panes[0], "C-d"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while f
        .mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
        .lines()
        .count()
        != 3
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
            .lines()
            .collect::<Vec<_>>(),
        panes[1..]
    );
    f.fail(
        &["new", "local", "--no-fetch", "mixed", "--no-open"],
        "已存在",
    );
    assert_eq!(
        f.mux(&["list-panes", "-t", &target, "-F", "#{pane_dead}"])
            .lines()
            .collect::<Vec<_>>(),
        vec!["1", "1", "1"]
    );
}

#[test]
fn startup_commands_return_to_shell_and_ctrl_d_closes_panes() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".twig")).unwrap();
    let script = f.repo.join("start 'command.sh");
    fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > received\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    f.git(&["add", "start 'command.sh"]);
    f.git(&["commit", "-m", "startup script"]);
    let literal = "quote ' backslash \\ $(touch unwanted) ;\nsecond line";
    let layout = serde_json::json!({"tmux": {"panes": [
        {"command": ["./start 'command.sh", literal]},
        {"split": "right", "command": ["/usr/bin/false"]},
        {"split": "down", "command": ["/bin/sleep", "60"]}
    ]}});
    fs::write(
        f.repo.join(".twig/config.toml"),
        toml::to_string(&layout).unwrap(),
    )
    .unwrap();
    // Shell 初始化不读取真实用户 dotfiles。
    f.mux(&[
        "set-environment",
        "-g",
        "HOME",
        f._temp.path().to_str().unwrap(),
    ]);
    for (task, shell) in [
        ("sh", "/bin/sh"),
        ("bash", "/bin/bash"),
        ("zsh", "/bin/zsh"),
    ] {
        f.mux(&["set-option", "-t", &f.session(), "default-shell", shell]);
        f.ok(&["new", "local", "--no-fetch", task, "--no-open"]);
        let target = format!("{}:{task}", f.session());
        let output = f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"]);
        let panes: Vec<_> = output.lines().collect();
        assert_eq!(panes.len(), 3);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fs::read_to_string(f.root.join(task).join("received"))
            .ok()
            .as_deref()
            != Some(literal)
            || f.mux(&[
                "display-message",
                "-p",
                "-t",
                panes[2],
                "#{pane_current_command}",
            ])
            .trim()
                != "sleep"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{shell} 的启动命令未按预期执行"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!f.root.join(task).join("unwanted").exists());
        f.mux(&["send-keys", "-t", panes[2], "C-c"]);
        for (index, pane) in panes.iter().enumerate() {
            f.mux(&[
                "send-keys",
                "-t",
                pane,
                "-l",
                &format!("printf 'ready' > after-{index}"),
            ]);
            f.mux(&["send-keys", "-t", pane, "Enter"]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while fs::read_to_string(f.root.join(task).join(format!("after-{index}")))
                .ok()
                .as_deref()
                != Some("ready")
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "{shell} 在命令结束后未接受输入"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            f.mux(&["send-keys", "-t", pane, "C-d"]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while f
                .mux(&["list-panes", "-a", "-F", "#{pane_id}"])
                .lines()
                .any(|id| id == *pane)
            {
                assert!(std::time::Instant::now() < deadline, "Ctrl-D 未关闭 Pane");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn startup_shells_inherit_global_options_and_honor_session_overrides() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".twig")).unwrap();
    fs::write(
        f.repo.join(".twig/config.toml"),
        "[tmux]\npanes = [{}, {split = 'right'}]",
    )
    .unwrap();
    f.mux(&["set-option", "-g", "default-shell", "/bin/sh"]);
    for (task, shell) in [("inherited", "/bin/sh"), ("override", "/bin/bash")] {
        if task == "override" {
            f.mux(&["set-option", "-t", &f.session(), "default-shell", shell]);
        }
        f.ok(&["new", "local", "--no-fetch", task, "--no-open"]);
        let panes = f.mux(&[
            "list-panes",
            "-t",
            &format!("{}:{task}", f.session()),
            "-F",
            "#{pane_id}",
        ]);
        assert_eq!(panes.lines().count(), 2);
        for (index, pane) in panes.lines().enumerate() {
            let command = f.mux(&["display-message", "-p", "-t", pane, "#{pane_start_command}"]);
            assert!(command.contains(shell), "Shell 启动命令错误：{command}");
            let marker = format!("shell-ready-{index}");
            f.mux(&[
                "send-keys",
                "-t",
                pane,
                "-l",
                &format!("printf 'ready' > {marker}"),
            ]);
            f.mux(&["send-keys", "-t", pane, "Enter"]);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let path = f.root.join(task).join(marker);
            while fs::read_to_string(&path).ok().as_deref() != Some("ready") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "Shell 未执行输入的命令"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(
                f.mux(&["display-message", "-p", "-t", pane, "#{pane_dead}"])
                    .trim(),
                "0"
            );
        }
    }
}

#[test]
fn startup_layout_precedence_commands_and_reopen() {
    let f = Fixture::new();
    let global = fs::read_to_string(f.registry_path()).unwrap();
    fs::write(
        f.registry_path(),
        format!(
            "[tmux]\npanes = [{{}}, {{ split = 'right', percent = 35, focus = true }}]\n{global}"
        ),
    )
    .unwrap();
    success(f.registered(&["new", "local", "--no-fetch", "global", "--no-open"]));
    let target = format!("{}:global", f.session());
    assert_eq!(
        f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
            .lines()
            .count(),
        2
    );
    assert_eq!(
        f.mux(&[
            "list-panes",
            "-t",
            &format!("{}:main", f.session()),
            "-F",
            "#{pane_id}"
        ])
        .lines()
        .count(),
        1
    );
    // 取消登记和重新登记不能丢失用户启动配置。
    f.ok(&["rm", "local"]);
    let saved: toml::Value =
        toml::from_str(&fs::read_to_string(f.registry_path()).unwrap()).unwrap();
    assert_eq!(saved["tmux"]["panes"].as_array().unwrap().len(), 2);
    f.ok(&["add", "local", "--path", f.repo.to_str().unwrap()]);
    fs::create_dir(f.repo.join(".twig")).unwrap();
    let local = f.repo.join(".twig/config.toml");
    fs::write(&local, "[tmux]\npanes = [{ command = ['/bin/echo', 'literal $(touch unwanted) ;'], focus = true }, { split = 'down', percent = 30 }]\n").unwrap();
    success(f.registered(&["new", "local", "--no-fetch", "configured", "--no-open"]));
    let target = format!("{}:configured", f.session());
    let panes = f.mux(&[
        "list-panes",
        "-t",
        &target,
        "-F",
        "#{pane_id}\t#{pane_active}\t#{pane_start_path}",
    ]);
    let first = panes.lines().next().unwrap().split('\t').next().unwrap();
    assert!(panes.lines().next().unwrap().contains("\t1\t"));
    assert!(
        panes
            .lines()
            .all(|line| line.ends_with(f.root.join("configured").to_str().unwrap()))
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let output = f.mux(&["capture-pane", "-p", "-S", "-", "-t", first]);
        if output
            .lines()
            .any(|line| line.trim() == "literal $(touch unwanted) ;")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "未看到命令输出：{output}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        f.mux(&["display-message", "-p", "-t", first, "#{pane_dead}"])
            .trim(),
        "0"
    );
    assert!(!f.root.join("configured/unwanted").exists());
    fs::write(&local, "[tmux]\npanes = []\n").unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "configured", "--no-open"],
        "已存在",
    );
    assert_eq!(
        f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
            .lines()
            .count(),
        2
    );
    f.ok(&["stop", "local", "configured", "-y"]);
    f.fail(&["open", "local", "configured"], "工作台需要交互式终端");
    assert_eq!(
        f.mux(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
            .lines()
            .count(),
        1
    );
    fs::write(&local, "# 未定义 tmux，继承用户配置\n").unwrap();
    f.ok(&["new", "local", "--no-fetch", "fallback", "--no-open"]);
    assert_eq!(
        f.mux(&[
            "list-panes",
            "-t",
            &format!("{}:fallback", f.session()),
            "-F",
            "#{pane_id}"
        ])
        .lines()
        .count(),
        2
    );
}

#[test]
fn startup_rejects_invalid_configuration_without_creating_window() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".twig")).unwrap();
    for (index, text) in [
        "[tmux]\npanes = [{command = []}]",
        "[tmux]\npanes = [{}, {split = 'right', percent = 0}]",
        "[tmux]\npanes = [{focus = true}, {split = 'down', focus = true}]",
        "[tmux]\npanes = [{unknown = true}]",
        "[tmux]\npanes = [{mode = 'process'}]",
        "[tmux]\npanes = [{mode = 'unknown'}]",
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(f.repo.join(".twig/config.toml"), text).unwrap();
        if index == 0 {
            f.fail(
                &["new", "local", "--no-fetch", "invalid", "--no-open"],
                "配置",
            );
        } else {
            f.fail(&["open", "local", "invalid"], "配置");
        }
        assert!(
            !f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_name}"])
                .lines()
                .any(|n| n == "invalid")
        );
    }
    fs::write(
        f.repo.join(".twig/config.toml"),
        "[tmux]\npanes = [{command = ['/usr/bin/false']}]",
    )
    .unwrap();
    f.fail(&["open", "local", "invalid"], "工作台需要交互式终端");
}

#[test]
fn startup_partial_failure_keeps_window_and_does_not_run_commands() {
    let f = Fixture::new();
    fs::create_dir(f.repo.join(".twig")).unwrap();
    let mut panes = vec!["{command = ['/usr/bin/touch', 'must-not-run']}"; 1];
    panes.extend(std::iter::repeat_n("{split = 'down'}", 30));
    fs::write(
        f.repo.join(".twig/config.toml"),
        format!("[tmux]\npanes = [{}]", panes.join(",")),
    )
    .unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "partial", "--no-open"],
        "初始化失败",
    );
    assert!(!f.root.join("partial/must-not-run").exists());
    f.fail(
        &["new", "local", "--no-fetch", "partial", "--no-open"],
        "已存在",
    );
    f.fail(&["open", "local", "partial"], "初始化未完成");
    fs::write(f.repo.join(".twig/config.toml"), "[tmux]\npanes = []").unwrap();
    f.ok(&["stop", "local", "partial", "-y"]);
    f.fail(&["open", "local", "partial"], "工作台需要交互式终端");
}

#[test]
fn duplicate_new_preserves_state_branches_windows_and_usage() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "auth", "--no-open"]);
    let state_path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    for stopped in [false, true] {
        if stopped {
            f.ok(&["stop", "local", "auth", "-y"]);
        }
        let state = fs::read(&state_path).unwrap();
        let registry = fs::read(f.registry_path()).unwrap();
        let refs = f.git(&["show-ref"]);
        let windows = f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_id}"]);
        for args in [
            vec!["new", "local", "--no-fetch", "auth"],
            vec![
                "new",
                "local",
                "--no-fetch",
                "auth",
                "--no-open",
                "--base",
                "absent",
            ],
            vec![
                "new",
                "local",
                "auth",
                "--no-open",
                "--expected-branch",
                "invalid",
            ],
        ] {
            f.fail(&args, "已存在");
            assert_eq!(fs::read(&state_path).unwrap(), state);
            assert_eq!(fs::read(f.registry_path()).unwrap(), registry);
            assert_eq!(f.git(&["show-ref"]), refs);
            assert_eq!(
                f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_id}"]),
                windows
            );
        }
    }
}

#[test]
fn lifecycle_and_recovery() {
    let f = Fixture::new();
    f.ok(&["add", "local", "--path", f.repo.to_str().unwrap()]);
    f.ok(&["new", "local", "--no-fetch", "--no-open", "auth"]);
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "auth"],
        "已存在",
    );
    f.ok(&["new", "local", "--no-fetch", "--no-open", "search"]);
    assert_eq!(
        f.git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        3
    );
    assert_eq!(
        f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_id}"])
            .lines()
            .count(),
        3
    );
    success(
        f.command(&f.root.join("auth"))
            .args(["ls", "local"])
            .output()
            .unwrap(),
    );
    f.ok(&["stop", "local", "auth", "-y"]);
    assert!(f.root.join("auth").is_dir());
    assert!(f.ok(&["ls", "local"]).contains("Closed"));
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "auth"],
        "已存在",
    );
    // open 的非交互工作台会失败，但必须已经重建窗口。
    f.ok(&["stop", "local", "auth", "-y"]);
    let _ = f.run(&["open", "local", "auth"]);
    assert!(
        f.mux(&["list-windows", "-t", &f.session(), "-F", "#{@twig-feature}"])
            .lines()
            .any(|line| line == "auth")
    );
    f.ok(&["stop", "local", "auth", "-y"]);
    f.ok(&["rm", "local", "auth", "-y"]);
    f.ok(&["rm", "local", "auth", "-y"]);
    assert!(!f.root.join("auth").exists());
    assert!(f.root.join("search").is_dir());
    f.mux(&["kill-server"]);
    assert!(f.ok(&["ls", "local"]).contains("Closed"));
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "search"],
        "已存在",
    );
}

#[test]
fn refuses_dirty_unmerged_and_busy_removal() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "auth"]);
    let path = f.root.join("auth");
    fs::write(path.join("notes"), "unsaved").unwrap();
    f.fail(&["rm", "local", "auth", "-y"], "未提交");
    success(
        Command::new("git")
            .current_dir(&path)
            .args(["add", "notes"])
            .output()
            .unwrap(),
    );
    success(
        Command::new("git")
            .current_dir(&path)
            .args(["commit", "-m", "work"])
            .output()
            .unwrap(),
    );
    f.fail(&["rm", "local", "auth", "-y"], "尚未合并");
    let target = format!("{}:auth", f.session());
    f.mux(&["send-keys", "-t", &target, "sleep 60", "Enter"]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    f.fail(&["rm", "local", "auth", "--keep-branch", "-y"], "活动进程");
    f.ok(&["stop", "local", "auth", "-y"]);
    f.ok(&["rm", "local", "auth", "--keep-branch", "-y"]);
    assert!(f.git(&["branch", "--list", "auth"]).contains("auth"));
}

#[test]
fn detects_conflicts_and_preserves_partial_creation() {
    let f = Fixture::new();
    f.git(&["branch", "existing"]);
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "existing"],
        "分支已存在",
    );
    fs::create_dir(f.root.join("occupied")).unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "occupied"],
        "路径已存在",
    );
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "../escape"],
        "名称须",
    );
    let mut command = f.command(&f.repo);
    command.env("TWIG_FAIL_TMUX", "1");
    support::daemon::ensure(&command, &f.registry_path());
    let out = command
        .args(["new", "local", "--no-fetch", "--no-open", "partial"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(f.root.join("partial").is_dir());
    support::daemon::stop_under(f._temp.path());
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "partial"],
        "已存在",
    );
    f.fail(&["open", "local", "partial"], "工作台需要交互式终端");
    assert_eq!(
        f.git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
    // 手动切换分支后拒绝删除。
    success(
        Command::new("git")
            .current_dir(f.root.join("partial"))
            .args(["switch", "-c", "other"])
            .output()
            .unwrap(),
    );
    f.fail(&["rm", "local", "partial", "-y"], "分支已改变");
    assert!(f.ok(&["ls", "local"]).contains("Mismatch"));
}

#[test]
fn ignored_files_and_missing_directory_are_protected() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "ignored"]);
    fs::write(f.repo.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(f.root.join("ignored/cache")).unwrap();
    fs::write(f.root.join("ignored/cache/file"), "data").unwrap();
    f.fail(&["rm", "local", "ignored", "-y"], "被忽略");
    f.ok(&["new", "local", "--no-fetch", "--no-open", "lost"]);
    f.ok(&["stop", "local", "lost", "-y"]);
    fs::remove_dir_all(f.root.join("lost")).unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "lost"],
        "已存在",
    );
    f.git(&["worktree", "prune"]);
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "lost"],
        "已存在",
    );
    assert!(!f.root.join("lost").exists());
}

#[test]
fn merged_branch_cleanup_and_confirmation() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "done"]);
    success(
        Command::new("git")
            .current_dir(f.root.join("done"))
            .args(["commit", "--allow-empty", "-m", "done"])
            .output()
            .unwrap(),
    );
    f.git(&["merge", "--ff-only", "done"]);
    f.ok(&["stop", "local", "done", "-y"]);
    f.fail(&["rm", "local", "done"], "已取消");
    f.ok(&["rm", "local", "done", "-y"]);
    assert!(f.git(&["branch", "--list", "done"]).trim().is_empty());
}

#[test]
fn numeric_names_renamed_windows_and_background_jobs() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "0"]);
    let windows = f.mux(&[
        "list-windows",
        "-t",
        &f.session(),
        "-F",
        "#{window_id}\t#{window_name}\t#{@twig-feature}",
    ]);
    let id = windows
        .lines()
        .find(|l| l.ends_with("\t0\t0"))
        .unwrap()
        .split('\t')
        .next()
        .unwrap();
    f.mux(&["rename-window", "-t", id, "renamed"]);
    f.fail(&["new", "local", "--no-fetch", "--no-open", "0"], "已存在");
    assert_eq!(
        f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_id}"])
            .lines()
            .count(),
        2
    );
    f.mux(&["send-keys", "-t", id, "sleep 60 &", "Enter"]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    f.fail(&["rm", "local", "0", "-y"], "活动进程");
    f.ok(&["stop", "local", "0", "-y"]);
    f.ok(&["rm", "local", "0", "-y"]);
}

#[test]
fn upstream_and_foreign_window_protection() {
    let f = Fixture::new();
    f.git(&["branch", "upstream"]);
    f.ok(&["new", "local", "--no-fetch", "--no-open", "unpushed"]);
    f.git(&["branch", "--set-upstream-to=upstream", "unpushed"]);
    success(
        Command::new("git")
            .current_dir(f.root.join("unpushed"))
            .args(["commit", "--allow-empty", "-m", "unpushed"])
            .output()
            .unwrap(),
    );
    f.git(&["merge", "--ff-only", "unpushed"]);
    f.fail(&["rm", "local", "unpushed", "-y"], "未推送提交");
    f.mux(&["new-window", "-d", "-t", &f.session(), "-n", "foreign"]);
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "foreign"],
        "同名非 twig",
    );
    // prepare 在创建资源前发现外来窗口，不能留下半成品 Worktree。
    assert!(!f.root.join("foreign").exists());
    f.mux(&[
        "rename-window",
        "-t",
        &format!("{}:foreign", f.session()),
        "personal",
    ]);
    f.ok(&["new", "local", "--no-fetch", "--no-open", "foreign"]);
    f.fail(&["open", "local", "foreign"], "工作台需要交互式终端");
}

#[test]
fn rechecks_files_after_confirmation_prompt() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "race"]);
    f.ok(&["stop", "local", "race", "-y"]);
    fs::write(f.repo.join(".git/info/exclude"), "late-file\n").unwrap();
    let mut child = f
        .command(&f.repo)
        .args(["rm", "local", "race"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut prompt = Vec::new();
    let mut byte = [0];
    while !prompt.ends_with(b"[y/N] ") {
        assert_eq!(child.stdout.as_mut().unwrap().read(&mut byte).unwrap(), 1);
        prompt.push(byte[0]);
    }
    fs::write(f.root.join("race/late-file"), "save me").unwrap();
    child.stdin.take().unwrap().write_all(b"y\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(f.root.join("race/late-file").exists());
}

#[test]
fn new_and_open_inside_tmux_keep_the_outer_client_in_its_session() {
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "auth", "--no-open"]);
    f.mux(&["new-session", "-d", "-s", "driver", "-x", "140", "-y", "36"]);
    let mut client = Command::new(&f.tmux)
        .args(["-L", &f.socket, "-C", "attach-session", "-t", "driver"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let wait_screen = |needle: &str| {
        for _ in 0..200 {
            if f.mux(&["capture-pane", "-p", "-t", "driver:"])
                .contains(needle)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "未看到 {needle}: {}",
            f.mux(&["capture-pane", "-p", "-t", "driver:"])
        );
    };
    for (i, action) in [
        "open local auth",
        "new local fresh --no-fetch",
        "open local fresh",
    ]
    .iter()
    .enumerate()
    {
        let command = format!(
            "'{}' --config '{}' {action}; printf 'RETURN_{i}_%s\\n' DONE",
            env!("CARGO_BIN_EXE_twig").replace('\'', "'\\''"),
            f.registry_path().to_str().unwrap().replace('\'', "'\\''")
        );
        f.mux(&["send-keys", "-t", "driver:", &command, "Enter"]);
        wait_screen(" · 全局 Codex ");
        let sessions = f.mux(&["list-clients", "-F", "#{session_name}"]);
        assert!(sessions.lines().any(|session| session == "driver"));
        assert!(sessions.lines().any(|session| session == f.session()));
        assert!(
            !support::project_dir(&f.repo, &f.registry_path())
                .join("lock")
                .exists()
        );
        f.mux(&["send-keys", "-t", "driver:", "C-w", "d"]);
        wait_screen("任务 / ");
        f.mux(&["send-keys", "-t", "driver:", "q"]);
        wait_screen(&format!("RETURN_{i}_DONE"));
    }
    f.ok(&["new", "local", "--no-fetch", "background", "--no-open"]);
    assert_eq!(
        f.mux(&["list-clients", "-F", "#{session_name}"]).trim(),
        "driver"
    );
    drop(client.stdin.take());
    client.wait().unwrap();
}

#[test]
fn new_preserves_environment_when_workbench_requires_a_terminal() {
    let f = Fixture::new();
    f.fail(
        &["new", "local", "--no-fetch", "auth"],
        "工作台需要交互式终端",
    );
    assert!(f.root.join("auth/.git").exists());
    assert!(
        !support::project_dir(&f.repo, &f.registry_path())
            .join("lock")
            .exists()
    );
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "auth"],
        "已存在",
    );
    assert!(f.ok(&["ls", "local"]).contains("Open"));
}

#[test]
fn slash_task_names_and_legacy_branches() {
    let f = Fixture::new();
    for name in ["feat/login", "fix/bug"] {
        f.ok(&["new", "local", "--no-fetch", "--no-open", name]);
        f.fail(&["new", "local", "--no-fetch", "--no-open", name], "已存在");
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(support::project_dir(&f.repo, &f.registry_path()).join("state.json"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(state[name]["branch"], name);
        assert_eq!(
            state[name]["path"],
            f.root.join(name).canonicalize().unwrap().to_str().unwrap()
        );
        assert!(
            f.mux(&["list-windows", "-t", &f.session(), "-F", "#{@twig-feature}"])
                .lines()
                .any(|line| line == name)
        );
        f.ok(&["stop", "local", name, "-y"]);
        let _ = f.run(&["open", "local", name]);
        f.ok(&["stop", "local", name, "-y"]);
        f.ok(&["rm", "local", name, "-y"]);
        assert!(!f.root.join(name).exists());
        assert!(f.git(&["branch", "--list", name]).is_empty());
    }
    f.ok(&["new", "local", "--no-fetch", "--no-open", "legacy"]);
    f.git(&["branch", "-m", "legacy", "feature/legacy"]);
    let path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["legacy"]["branch"] = "feature/legacy".into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "legacy"],
        "已存在",
    );
    assert!(f.ok(&["ls", "local"]).contains("feature/legacy"));
    f.ok(&["stop", "local", "legacy", "-y"]);
    f.ok(&["rm", "local", "legacy", "-y"]);
}

#[test]
fn slash_tasks_reject_unsafe_paths_before_recording() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    for name in [
        "../escape",
        "/absolute",
        "feat/../escape",
        "feat//login",
        "feat/",
        "feat/.hidden",
    ] {
        f.fail(&["new", "local", "--no-fetch", "--no-open", name], "名称须");
    }
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "HEAD"],
        "not a valid branch",
    );
    symlink(f._temp.path(), f.root.join("escape")).unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "escape/task"],
        "不是普通目录",
    );
    fs::create_dir_all(f.root.join("nested/.git")).unwrap();
    f.fail(
        &["new", "local", "--no-fetch", "--no-open", "nested/task"],
        "已有仓库",
    );
    assert!(
        !support::project_dir(&f.repo, &f.registry_path())
            .join("state.json")
            .exists()
    );
    assert!(!f._temp.path().join("task").exists());
}

#[test]
fn delete_ignored_is_explicit_and_still_protects_other_files() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "--no-open", "cache-task"]);
    let path = f.root.join("cache-task");
    fs::write(f.repo.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(path.join("cache")).unwrap();
    fs::write(path.join("cache/data"), "ignored data").unwrap();
    f.fail(&["rm", "local", "cache-task", "-y"], "被忽略");
    fs::write(path.join("unsaved"), "save me").unwrap();
    f.fail(
        &["rm", "local", "cache-task", "--delete-ignored", "-y"],
        "未提交",
    );
    success(
        Command::new("git")
            .current_dir(&path)
            .args(["add", "unsaved"])
            .output()
            .unwrap(),
    );
    f.fail(
        &["rm", "local", "cache-task", "--delete-ignored", "-y"],
        "未提交",
    );
    success(
        Command::new("git")
            .current_dir(&path)
            .args(["reset", "HEAD", "--", "unsaved"])
            .output()
            .unwrap(),
    );
    fs::remove_file(path.join("unsaved")).unwrap();
    f.fail(
        &[
            "rm",
            "local",
            "cache-task",
            "--delete-ignored",
            "--expected-ignored",
            "[]",
            "-y",
        ],
        "清单已变化",
    );
    assert!(path.join("cache/data").exists());
    f.ok(&["rm", "local", "cache-task", "--delete-ignored", "-y"]);
    assert!(!path.exists());
}

#[test]
fn global_state_shares_worktrees_and_isolates_data_roots_and_clones() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "shared", "--no-open"]);
    let dir = support::project_dir(&f.repo, &f.registry_path());
    assert!(dir.join("state.json").is_file());
    assert!(!f.repo.join(".git/twig").exists());
    assert!(
        success(
            f.command(&f.root.join("shared"))
                .args(["ls", "local"])
                .output()
                .unwrap()
        )
        .contains("shared")
    );
    // 同目录的不同登记表仍共用项目状态；命令始终通过登记别名定位。
    let sibling = f.registry_path().with_file_name("other.toml");
    fs::copy(f.registry_path(), &sibling).unwrap();
    assert!(
        success(
            f.command_with_registry(&f.repo, &sibling)
                .args(["ls", "local"])
                .output()
                .unwrap()
        )
        .contains("shared")
    );
    assert!(sibling.exists());
    // 切换数据根目录后不能看见原状态。
    let isolated = f._temp.path().join("isolated/config.toml");
    fs::create_dir_all(isolated.parent().unwrap()).unwrap();
    fs::copy(f.registry_path(), &isolated).unwrap();
    let out = f
        .command_with_registry(&f.repo, &isolated)
        .args(["ls", "local"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("请先执行 twig add"));
    assert!(dir.join("state.json").is_file());
    let clone = f._temp.path().join("clone");
    f.git(&["clone", f.repo.to_str().unwrap(), clone.to_str().unwrap()]);
    success(
        f.command(&clone)
            .args(["add", "clone", "--path", clone.to_str().unwrap()])
            .output()
            .unwrap(),
    );
    let clone_dir = support::project_dir(&clone, &f.registry_path());
    assert_ne!(dir, clone_dir);
    assert!(!success(f.command(&clone).args(["ls", "clone"]).output().unwrap()).contains("shared"));
    // 即使误放了另一个仓库的配置，也必须拒绝使用。
    fs::copy(dir.join("config.json"), clone_dir.join("config.json")).unwrap();
    let out = f.command(&clone).args(["ls", "clone"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("仓库归属不一致"));
}

#[test]
fn default_global_storage_ignores_and_preserves_old_files() {
    let f = Fixture::new();
    let home = f._temp.path().join("home");
    let old = f.repo.join(".git/twig");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("config.json"), "invalid old config").unwrap();
    fs::write(old.join("state.json"), "invalid old state").unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
    c.current_dir(&f.repo)
        .env("HOME", &home)
        .env(
            "PATH",
            format!("{}:{}", f.bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("SHELL", "/bin/sh")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    support::daemon::ensure(&c, &home.join(".twig/config.toml"));
    success(
        c.args(["add", "default", "--path", f.repo.to_str().unwrap()])
            .output()
            .unwrap(),
    );
    let registry = home.join(".twig/config.toml");
    assert!(registry.is_file());
    assert!(
        support::project_dir(&f.repo, &registry)
            .join("config.json")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(old.join("config.json")).unwrap(),
        "invalid old config"
    );
    assert_eq!(
        fs::read_to_string(old.join("state.json")).unwrap(),
        "invalid old state"
    );
}

#[test]
fn global_commands_ignore_cwd_and_project_removal_preserves_resources() {
    let f = Fixture::new();
    success(f.registered(&["new", "local", "--no-fetch", "auth", "--no-open"]));
    assert!(success(f.registered(&["ls"])).contains("local"));
    assert!(success(f.registered(&["ls", "local"])).contains("auth"));
    let state_path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    let before = fs::read(&state_path).unwrap();
    let registry = fs::read(f.registry_path()).unwrap();
    for args in [
        vec!["new", "local"],
        vec!["open", "local"],
        vec!["stop", "local"],
        vec!["rm"],
        vec!["rm", "local", "--keep-branch"],
        vec!["rm", "local", "--delete-ignored"],
        vec!["rm", "local", "--target", "main"],
        vec!["init"],
        vec!["config", "list"],
        vec!["clone", "local"],
        vec!["--repo", "local", "ls"],
    ] {
        assert!(!f.registered(&args).status.success(), "应拒绝 {args:?}");
        assert_eq!(fs::read(f.registry_path()).unwrap(), registry);
    }
    success(f.registered(&["rm", "local"]));
    assert!(success(f.registered(&["ls"])).is_empty());
    assert!(f.root.join("auth/.git").exists());
    assert_eq!(fs::read(&state_path).unwrap(), before);
    assert!(
        f.mux(&["list-sessions", "-F", "#{session_name}"])
            .contains(&f.session())
    );
    assert!(!f.registered(&["ls", "local"]).status.success());
    success(f.registered(&["add", "renamed", "--path", f.repo.to_str().unwrap()]));
    assert!(success(f.registered(&["ls", "renamed"])).contains("auth"));
}

#[test]
fn add_reuses_alias_and_rejects_conflicts_without_overwriting() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-fetch", "auth", "--no-open"]);
    let before = fs::read(f.registry_path()).unwrap();
    success(f.registered(&[
        "add",
        "another",
        "--path",
        f.root.join("auth").to_str().unwrap(),
    ]));
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    let other = f._temp.path().join("other");
    f.git(&["clone", f.repo.to_str().unwrap(), other.to_str().unwrap()]);
    let out = f.registered(&["add", "local", "--path", other.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("已指向其他路径"));
    assert!(
        !support::project_dir(&other, &f.registry_path())
            .join("config.json")
            .exists()
    );
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    f.fail(
        &["add", "bad name", "--path", other.to_str().unwrap()],
        "名称须",
    );
    f.fail(
        &["add", "missing", "--path", "/twig-nonexistent-test-path"],
        "仓库目录不存在",
    );
    f.fail(
        &[
            "add",
            "local",
            "--path",
            f.repo.to_str().unwrap(),
            "--base",
            "missing",
        ],
        "项目已初始化",
    );
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    fs::write(f.registry_path(), "invalid {").unwrap();
    assert!(
        !f.registered(&["add", "other", "--path", other.to_str().unwrap()])
            .status
            .success()
    );
    assert!(!f.registered(&["rm", "local"]).status.success());
    assert_eq!(fs::read_to_string(f.registry_path()).unwrap(), "invalid {");
}

#[test]
fn add_resolves_relative_paths_and_can_retry_partial_initialization() {
    let f = Fixture::new();
    let other = f._temp.path().join("other");
    f.git(&["clone", f.repo.to_str().unwrap(), other.to_str().unwrap()]);
    let wrapper = f.bin.join("tmux");
    let original = fs::read(&wrapper).unwrap();
    fs::write(&wrapper, "#!/bin/sh\necho injected-failure >&2\nexit 71\n").unwrap();
    let before = fs::read(f.registry_path()).unwrap();
    let out = f
        .command_with_registry(f._temp.path(), Path::new("user-config/config.toml"))
        .args(["add", "other", "--path", "other", "--root", "other-tasks"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let config_path = support::project_dir(&other, &f.registry_path()).join("config.json");
    let saved = fs::read(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        PathBuf::from(config["root"].as_str().unwrap()),
        f._temp.path().join("other-tasks").canonicalize().unwrap()
    );
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    fs::write(&wrapper, original).unwrap();
    success(f.registered(&["add", "other", "--path", "other"]));
    assert_eq!(fs::read(&config_path).unwrap(), saved);
    success(f.registered(&["new", "other", "--no-fetch", "task", "--no-open"]));
    assert!(f._temp.path().join("other-tasks/task/.git").exists());
}

#[test]
fn listing_missing_registry_is_readonly_and_requires_no_repository() {
    let f = Fixture::new();
    let registry = f._temp.path().join("missing/config.toml");
    assert!(
        success(
            f.command_with_registry(f._temp.path(), &registry)
                .arg("ls")
                .output()
                .unwrap()
        )
        .is_empty()
    );
    assert!(
        !f.command_with_registry(f._temp.path(), &registry)
            .args(["rm", "unknown"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!registry.exists());
    assert!(!registry.parent().unwrap().join("projects").exists());
}

#[test]
fn disconnected_submit_and_retry_create_only_one_task() {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let f = Fixture::new();
    let status: Value = serde_json::from_str(&f.ok(&["daemon", "status"])).unwrap();
    let socket = status["socket"].as_str().unwrap();
    let rpc = |method: &str, params: Value, read: bool| -> Value {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            json!({"version":status["version"],"id":"client","method":method,"params":params})
        )
        .unwrap();
        if !read {
            return Value::Null;
        }
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert!(value["error"].is_null(), "{value}");
        value["result"].clone()
    };
    let plan = rpc(
        "operation.prepare",
        json!({"New":{"project":"local","name":"detached","base":null,"no_open":true,"agent":false,"expected_branch":null}}),
        true,
    );
    let params = json!({"plan":plan["id"],"request":"lost-response","confirmed":true});
    rpc("operation.submit", params.clone(), false);
    let id = rpc("operation.submit", params, true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let record = rpc("operation.get", json!("lost-response"), true);
        if record["status"] == "succeeded" {
            assert_eq!(record["id"], id);
            assert!(record["stages"].as_array().unwrap().len() >= 3);
            break;
        }
        assert!(
            record["status"] == "queued" || record["status"] == "running",
            "{record}"
        );
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(f.root.join("detached").is_dir());
    assert_eq!(
        f.git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
}

#[test]
fn new_fetches_selected_upstream_without_changing_local_base_or_files() {
    let f = Fixture::new();
    let remote = f._temp.path().join("remote.git");
    let writer = f._temp.path().join("writer");
    f.git(&[
        "clone",
        "--bare",
        f.repo.to_str().unwrap(),
        remote.to_str().unwrap(),
    ]);
    f.git(&["clone", remote.to_str().unwrap(), writer.to_str().unwrap()]);
    f.git(&["-C", writer.to_str().unwrap(), "checkout", "-b", "trunk"]);
    fs::write(writer.join("remote-only"), "latest remote").unwrap();
    f.git(&["-C", writer.to_str().unwrap(), "add", "remote-only"]);
    f.git(&[
        "-C",
        writer.to_str().unwrap(),
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "remote update",
    ]);
    f.git(&["-C", writer.to_str().unwrap(), "push", "origin", "trunk"]);
    let remote_commit = f.git(&["-C", writer.to_str().unwrap(), "rev-parse", "HEAD"]);
    f.git(&["remote", "add", "upstream", remote.to_str().unwrap()]);
    // 远端分支名与本地 base 不同，且没有本地远端跟踪引用。
    f.git(&["branch", "release"]);
    f.git(&["config", "branch.release.remote", "upstream"]);
    f.git(&["config", "branch.release.merge", "refs/heads/trunk"]);
    f.git(&["commit", "--allow-empty", "-m", "local only"]);
    fs::write(f.repo.join("unsaved"), "keep me").unwrap();
    let local_head = f.git(&["rev-parse", "HEAD"]);
    let local_base = f.git(&["rev-parse", "release"]);
    success(f.run(&["new", "local", "fresh", "--base", "release", "--no-open"]));
    assert_eq!(f.git(&["rev-parse", "fresh"]), remote_commit);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), local_head);
    assert_eq!(f.git(&["rev-parse", "release"]), local_base);
    assert_eq!(
        fs::read_to_string(f.repo.join("unsaved")).unwrap(),
        "keep me"
    );
    assert!(!f.repo.join("remote-only").exists());
    assert!(f.root.join("fresh/remote-only").exists());
    success(f.run(&["new", "local", "offline", "--no-fetch", "--no-open"]));
    assert_eq!(f.git(&["rev-parse", "offline"]), local_head);
}

#[test]
fn new_fetch_failure_leaves_no_task_record_and_can_retry_offline() {
    let f = Fixture::new();
    let state_path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    let before = fs::read(&state_path).ok();
    for configured in [false, true] {
        if configured {
            f.git(&[
                "remote",
                "add",
                "unavailable",
                f._temp.path().join("missing.git").to_str().unwrap(),
            ]);
            f.git(&["config", "branch.main.remote", "unavailable"]);
            f.git(&["config", "branch.main.merge", "refs/heads/main"]);
        }
        let out = f.run(&["new", "local", "retry", "--no-open"]);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(if configured {
                "更新基础分支 main"
            } else {
                "未配置远端 upstream"
            })
        );
        assert_eq!(fs::read(&state_path).ok(), before);
        assert!(!f.root.join("retry").exists());
        assert!(
            f.git(&["for-each-ref", "--format=%(refname)", "refs/heads/retry"])
                .trim()
                .is_empty()
        );
    }
    success(f.run(&["new", "local", "retry", "--no-fetch", "--no-open"]));
    assert!(f.root.join("retry/.git").exists());
}
