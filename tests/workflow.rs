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
            "init",
            "--name",
            "local",
            "--root",
            f.root.to_str().unwrap(),
        ]);
        f
    }
    fn command(&self, cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
        c.arg("--config").arg(self.registry_path());
        c.current_dir(cwd)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("SHELL", "/bin/sh")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
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
        let v: serde_json::Value =
            serde_json::from_slice(&fs::read(self.repo.join(".git/twig/config.json")).unwrap())
                .unwrap();
        v["session"].as_str().unwrap().to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
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
fn lifecycle_and_recovery() {
    let f = Fixture::new();
    f.ok(&["init"]);
    f.ok(&["new", "--no-open", "auth"]);
    f.ok(&["new", "--no-open", "auth"]);
    f.ok(&["new", "--no-open", "search"]);
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
            .args(["ls"])
            .output()
            .unwrap(),
    );
    f.ok(&["stop", "auth", "-y"]);
    assert!(f.root.join("auth").is_dir());
    assert!(f.ok(&["ls"]).contains("Closed"));
    f.ok(&["new", "--no-open", "auth"]);
    // open 的非交互 attach 会失败，但必须已经重建窗口。
    f.ok(&["stop", "auth", "-y"]);
    let _ = f.run(&["open", "auth"]);
    assert!(
        f.mux(&["list-windows", "-t", &f.session(), "-F", "#{@twig-feature}"])
            .lines()
            .any(|line| line == "auth")
    );
    f.ok(&["stop", "auth", "-y"]);
    f.ok(&["rm", "auth", "-y"]);
    f.ok(&["rm", "auth", "-y"]);
    assert!(!f.root.join("auth").exists());
    assert!(f.root.join("search").is_dir());
    f.mux(&["kill-server"]);
    assert!(f.ok(&["ls"]).contains("Closed"));
    f.ok(&["new", "--no-open", "search"]);
}

#[test]
fn refuses_dirty_unmerged_and_busy_removal() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "auth"]);
    let path = f.root.join("auth");
    fs::write(path.join("notes"), "unsaved").unwrap();
    f.fail(&["rm", "auth", "-y"], "未提交");
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
    f.fail(&["rm", "auth", "-y"], "尚未合并");
    let target = format!("{}:auth", f.session());
    f.mux(&["send-keys", "-t", &target, "sleep 60", "Enter"]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    f.fail(&["rm", "auth", "--keep-branch", "-y"], "活动进程");
    f.ok(&["stop", "auth", "-y"]);
    f.ok(&["rm", "auth", "--keep-branch", "-y"]);
    assert!(f.git(&["branch", "--list", "auth"]).contains("auth"));
}

#[test]
fn detects_conflicts_and_resumes_partial_creation() {
    let f = Fixture::new();
    f.git(&["branch", "existing"]);
    f.fail(&["new", "--no-open", "existing"], "分支已存在");
    fs::create_dir(f.root.join("occupied")).unwrap();
    f.fail(&["new", "--no-open", "occupied"], "路径已存在");
    f.fail(&["new", "--no-open", "../escape"], "名称须");
    let out = f
        .command(&f.repo)
        .env("TWIG_FAIL_TMUX", "1")
        .args(["new", "--no-open", "partial"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(f.root.join("partial").is_dir());
    f.ok(&["new", "--no-open", "partial"]);
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
    f.fail(&["rm", "partial", "-y"], "分支已改变");
    assert!(f.ok(&["ls"]).contains("Mismatch"));
}

#[test]
fn ignored_files_and_missing_directory_are_protected() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "ignored"]);
    fs::write(f.repo.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(f.root.join("ignored/cache")).unwrap();
    fs::write(f.root.join("ignored/cache/file"), "data").unwrap();
    f.fail(&["rm", "ignored", "-y"], "被忽略");
    f.ok(&["new", "--no-open", "lost"]);
    f.ok(&["stop", "lost", "-y"]);
    fs::remove_dir_all(f.root.join("lost")).unwrap();
    f.fail(&["new", "--no-open", "lost"], "目录丢失");
    f.git(&["worktree", "prune"]);
    f.ok(&["new", "--no-open", "lost"]);
}

#[test]
fn merged_branch_cleanup_and_confirmation() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "done"]);
    success(
        Command::new("git")
            .current_dir(f.root.join("done"))
            .args(["commit", "--allow-empty", "-m", "done"])
            .output()
            .unwrap(),
    );
    f.git(&["merge", "--ff-only", "done"]);
    f.ok(&["stop", "done", "-y"]);
    f.fail(&["rm", "done"], "已取消");
    f.ok(&["rm", "done", "-y"]);
    assert!(f.git(&["branch", "--list", "done"]).trim().is_empty());
}

#[test]
fn numeric_names_renamed_windows_and_background_jobs() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "0"]);
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
    f.ok(&["new", "--no-open", "0"]);
    assert_eq!(
        f.mux(&["list-windows", "-t", &f.session(), "-F", "#{window_id}"])
            .lines()
            .count(),
        2
    );
    f.mux(&["send-keys", "-t", id, "sleep 60 &", "Enter"]);
    std::thread::sleep(std::time::Duration::from_millis(150));
    f.fail(&["rm", "0", "-y"], "活动进程");
    f.ok(&["stop", "0", "-y"]);
    f.ok(&["rm", "0", "-y"]);
}

#[test]
fn upstream_and_foreign_window_protection() {
    let f = Fixture::new();
    f.git(&["branch", "upstream"]);
    f.ok(&["new", "--no-open", "unpushed"]);
    f.git(&["branch", "--set-upstream-to=upstream", "unpushed"]);
    success(
        Command::new("git")
            .current_dir(f.root.join("unpushed"))
            .args(["commit", "--allow-empty", "-m", "unpushed"])
            .output()
            .unwrap(),
    );
    f.git(&["merge", "--ff-only", "unpushed"]);
    f.fail(&["rm", "unpushed", "-y"], "未推送提交");
    f.mux(&["new-window", "-d", "-t", &f.session(), "-n", "foreign"]);
    f.fail(&["new", "--no-open", "foreign"], "同名非 twig");
    assert!(f.root.join("foreign").is_dir());
    f.mux(&[
        "rename-window",
        "-t",
        &format!("{}:foreign", f.session()),
        "personal",
    ]);
    f.ok(&["new", "--no-open", "foreign"]);
}

#[test]
fn rechecks_files_after_confirmation_prompt() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "race"]);
    f.ok(&["stop", "race", "-y"]);
    fs::write(f.repo.join(".git/info/exclude"), "late-file\n").unwrap();
    let mut child = f
        .command(&f.repo)
        .args(["rm", "race"])
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
fn new_and_open_switch_an_attached_client_between_sessions() {
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "auth"]);
    f.mux(&[
        "new-session",
        "-d",
        "-s",
        "driver",
        "-c",
        f.repo.to_str().unwrap(),
    ]);
    let mut client = Command::new(&f.tmux)
        .args(["-L", &f.socket, "-C", "attach-session", "-t", "driver"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let wait_for = |session: &str| {
        for _ in 0..100 {
            if f.mux(&["list-clients", "-F", "#{session_name}"])
                .lines()
                .any(|line| line == session)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("客户端未进入 {session}");
    };
    wait_for("driver");
    for action in ["open auth", "new fresh", "new fresh"] {
        f.mux(&["switch-client", "-t", "driver"]);
        wait_for("driver");
        let command = format!(
            "'{}' {action}",
            env!("CARGO_BIN_EXE_twig").replace('\'', "'\\''")
        );
        f.mux(&["send-keys", "-t", "driver:", &command, "Enter"]);
        wait_for(&f.session());
        let expected = if action == "open auth" {
            "auth"
        } else {
            "fresh"
        };
        assert_eq!(
            f.mux(&[
                "display-message",
                "-p",
                "-t",
                &format!("{}:", f.session()),
                "#{@twig-feature}"
            ])
            .trim(),
            expected
        );
        assert!(!f.repo.join(".git/twig/lock").exists());
    }
    f.ok(&["new", "--no-open", "background"]);
    assert_eq!(
        f.mux(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:", f.session()),
            "#{@twig-feature}"
        ])
        .trim(),
        "fresh"
    );
    drop(client.stdin.take());
    client.wait().unwrap();
}

#[test]
fn new_preserves_environment_when_attach_fails_without_a_terminal() {
    let f = Fixture::new();
    f.fail(&["new", "auth"], "tmux 进入失败；环境已保留");
    assert!(f.root.join("auth/.git").exists());
    assert!(!f.repo.join(".git/twig/lock").exists());
    f.ok(&["new", "--no-open", "auth"]);
    assert!(f.ok(&["ls"]).contains("Open"));
}

#[test]
fn registry_clone_multiple_remotes_and_repo_selection() {
    let f = Fixture::new();
    f.git(&["branch", "develop"]);
    let destination = f._temp.path().join("cloned project");
    success(f.registered(&[
        "config",
        "add",
        "app",
        "--url",
        f.repo.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
        "--main",
        "develop",
        "--remote",
        "fork",
    ]));
    success(f.registered(&[
        "config",
        "remote",
        "add",
        "app",
        "upstream",
        f.repo.to_str().unwrap(),
    ]));
    assert!(!destination.exists(), "登记不应 clone 或创建目标目录");
    assert!(success(f.registered(&["config", "list"])).contains("app"));
    assert!(
        success(f.registered(&["config", "show", "app"])).contains("default_remote = \"fork\"")
    );
    success(f.registered(&["clone", "app"]));
    success(f.registered(&["clone", "app"]));
    let git_at = |args: &[&str]| {
        success(
            Command::new("git")
                .current_dir(&destination)
                .args(args)
                .output()
                .unwrap(),
        )
    };
    assert_eq!(git_at(&["branch", "--show-current"]).trim(), "develop");
    assert_eq!(
        git_at(&["remote"]).lines().collect::<Vec<_>>(),
        vec!["fork", "upstream"]
    );
    assert_eq!(
        git_at(&["config", "--get", "remote.upstream.url"]).trim(),
        f.repo.to_str().unwrap()
    );
    success(f.registered(&["--repo", "app", "new", "--no-open", "auth"]));
    assert!(success(f.registered(&["ls", "--repo", "app"])).contains("auth"));
    let base = git_at(&["rev-parse", "develop"]);
    assert_eq!(git_at(&["rev-parse", "auth"]), base);
    success(f.registered(&["config", "remove", "app"]));
    assert!(destination.join(".git").exists());
    assert!(git_at(&["branch", "--list", "auth"]).contains("auth"));
    assert!(!f.registered(&["--repo", "app", "ls"]).status.success());
}

#[test]
fn registry_rejects_conflicts_and_preserves_configuration() {
    let f = Fixture::new();
    let destination = f._temp.path().join("occupied");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("precious"), "keep").unwrap();
    let add = [
        "config",
        "add",
        "app",
        "--url",
        f.repo.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
    ];
    success(f.registered(&add));
    let before = fs::read(f.registry_path()).unwrap();
    assert!(!f.registered(&add).status.success());
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    assert!(!f.registered(&["clone", "app"]).status.success());
    assert_eq!(
        fs::read_to_string(destination.join("precious")).unwrap(),
        "keep"
    );
    assert!(
        !f.registered(&["config", "remote", "remove", "app", "origin"])
            .status
            .success()
    );
    assert!(
        !f.registered(&["config", "remote", "default", "app", "missing"])
            .status
            .success()
    );
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    success(f.registered(&[
        "config",
        "remote",
        "add",
        "app",
        "upstream",
        f.repo.to_str().unwrap(),
    ]));
    success(f.registered(&["config", "remote", "default", "app", "upstream"]));
    success(f.registered(&["config", "remote", "remove", "app", "origin"]));
    assert!(
        success(f.registered(&["config", "show", "app"])).contains("default_remote = \"upstream\"")
    );
    fs::write(f.registry_path(), "invalid = [").unwrap();
    assert!(!f.registered(&["config", "remove", "app"]).status.success());
    assert_eq!(
        fs::read_to_string(f.registry_path()).unwrap(),
        "invalid = ["
    );
}

#[test]
fn clone_detects_remote_and_main_conflicts_without_overwriting() {
    let f = Fixture::new();
    let destination = f._temp.path().join("clone");
    success(f.registered(&[
        "config",
        "add",
        "app",
        "--url",
        f.repo.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
    ]));
    success(f.registered(&["clone", "app"]));
    let git_at = |args: &[&str]| {
        success(
            Command::new("git")
                .current_dir(&destination)
                .args(args)
                .output()
                .unwrap(),
        )
    };
    success(f.registered(&[
        "config",
        "remote",
        "add",
        "app",
        "extra",
        "https://example.invalid/extra.git",
    ]));
    git_at(&[
        "remote",
        "set-url",
        "origin",
        "https://example.invalid/changed.git",
    ]);
    let out = f.registered(&["clone", "app"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("URL 与登记不符"));
    assert_eq!(git_at(&["remote"]).trim(), "origin");
    assert_eq!(
        git_at(&["config", "--get", "remote.origin.url"]).trim(),
        "https://example.invalid/changed.git"
    );
    git_at(&["remote", "set-url", "origin", f.repo.to_str().unwrap()]);
    let project_config = destination.join(".git/twig/config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&project_config).unwrap()).unwrap();
    config["base"] = "other".into();
    fs::write(&project_config, serde_json::to_vec(&config).unwrap()).unwrap();
    let out = f.registered(&["clone", "app"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("主分支与已有 twig 配置冲突"));
    assert_eq!(git_at(&["remote"]).trim(), "origin");
}

#[test]
fn registered_init_and_failed_clone_initialization_can_resume() {
    let f = Fixture::new();
    let destination = f._temp.path().join("recover clone");
    success(f.registered(&[
        "config",
        "add",
        "app",
        "--url",
        f.repo.to_str().unwrap(),
        "--path",
        destination.to_str().unwrap(),
    ]));
    // 让 clone 后的 tmux 初始化失败；Git 仓库必须保留。
    let wrapper = f.bin.join("tmux");
    let original = fs::read(&wrapper).unwrap();
    fs::write(&wrapper, "#!/bin/sh\necho injected-failure >&2\nexit 71\n").unwrap();
    assert!(!f.registered(&["clone", "app"]).status.success());
    assert!(destination.join(".git").exists());
    fs::write(&wrapper, original).unwrap();
    success(f.registered(&["clone", "app"]));
    success(f.registered(&["--repo", "app", "init"]));
    assert!(
        success(f.registered(&["--repo", "app", "init", "--base", "other"]))
            .contains("当前项目已经初始化")
    );
}

#[test]
fn init_registers_local_projects_and_reuses_worktree_registration() {
    let f = Fixture::new();
    let before = fs::read(f.registry_path()).unwrap();
    f.ok(&["init"]);
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    success(f.registered(&["--repo", "local", "ls"]));
    f.ok(&["new", "--no-open", "task"]);
    success(
        f.command(&f.root.join("task"))
            .args(["init"])
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    f.fail(&["clone", "local"], "未配置默认 remote");
    success(f.registered(&[
        "config",
        "remote",
        "add",
        "local",
        "origin",
        f.repo.to_str().unwrap(),
    ]));
    assert!(
        success(f.registered(&["config", "show", "local"])).contains("default_remote = \"origin\"")
    );

    let repo = f._temp.path().join("fresh");
    f.git(&["clone", f.repo.to_str().unwrap(), repo.to_str().unwrap()]);
    success(f.command(&repo).args(["init"]).output().unwrap());
    let entry = success(f.registered(&["config", "show", "fresh"]));
    assert!(entry.contains("main = \"main\""));
    assert!(!entry.contains("default_remote"));
    success(f.registered(&["--repo", "fresh", "ls"]));
}

#[test]
fn init_checks_registration_before_creating_resources() {
    let f = Fixture::new();
    let repo = f._temp.path().join("local");
    f.git(&["clone", f.repo.to_str().unwrap(), repo.to_str().unwrap()]);
    let before = fs::read(f.registry_path()).unwrap();
    let out = f.command(&repo).args(["init"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("已指向其他路径"));
    assert!(!repo.join(".git/twig/config.json").exists());
    assert!(!f._temp.path().join("local.worktrees").exists());
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    let out = f
        .command(&repo)
        .args(["init", "--name", "bad name"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!repo.join(".git/twig/config.json").exists());
    fs::write(f.registry_path(), "invalid = [").unwrap();
    assert!(
        !f.command(&repo)
            .args(["init", "--name", "other"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!repo.join(".git/twig/config.json").exists());
    fs::write(f.registry_path(), before).unwrap();
    success(
        f.command(&repo)
            .args(["init", "--name", "other"])
            .output()
            .unwrap(),
    );
    success(f.registered(&["--repo", "other", "ls"]));
}

#[test]
fn repeated_init_after_partial_failure_does_not_register_or_restore_session() {
    let f = Fixture::new();
    let repo = f._temp.path().join("fresh");
    f.git(&["clone", f.repo.to_str().unwrap(), repo.to_str().unwrap()]);
    success(
        Command::new("git")
            .current_dir(&repo)
            .args(["branch", "develop"])
            .output()
            .unwrap(),
    );
    let wrapper = f.bin.join("tmux");
    let original = fs::read(&wrapper).unwrap();
    fs::write(&wrapper, "#!/bin/sh\necho injected-failure >&2\nexit 71\n").unwrap();
    let before = fs::read(f.registry_path()).unwrap();
    assert!(
        !f.command(&repo)
            .args(["init", "--base", "develop", "--target", "main"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(repo.join(".git/twig/config.json").exists());
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    fs::write(wrapper, original).unwrap();
    success(f.command(&repo).args(["init"]).output().unwrap());
    assert_eq!(fs::read(f.registry_path()).unwrap(), before);
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join(".git/twig/config.json")).unwrap()).unwrap();
    assert_eq!(config["base"], "develop");
    assert_eq!(config["target"], "main");
}

#[test]
fn repeated_init_is_read_only_even_with_missing_registry_and_session() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "task"]);
    let config_path = f.repo.join(".git/twig/config.json");
    let before = fs::read(&config_path).unwrap();
    f.mux(&["kill-server"]);
    fs::remove_dir_all(f.registry_path().parent().unwrap()).unwrap();
    // 锁已存在仍须直接退出，不能尝试获取锁或读取配置内容。
    let lock = f.repo.join(".git/twig/lock");
    fs::write(&lock, "existing lock").unwrap();
    fs::write(f.bin.join("tmux"), "#!/bin/sh\nexit 71\n").unwrap();
    let unused_root = f._temp.path().join("unused");
    for cwd in [&f.repo, &f.root.join("task")] {
        let out = success(
            f.command(cwd)
                .args([
                    "init",
                    "--name",
                    "bad name",
                    "--base",
                    "missing",
                    "--target",
                    "missing",
                    "--root",
                    unused_root.to_str().unwrap(),
                ])
                .output()
                .unwrap(),
        );
        assert!(out.contains("当前项目已经初始化，无需重复执行。"));
    }
    assert_eq!(fs::read(config_path).unwrap(), before);
    assert_eq!(fs::read_to_string(lock).unwrap(), "existing lock");
    assert!(!f.registry_path().parent().unwrap().exists());
    assert!(!unused_root.exists());
}

#[test]
fn slash_task_names_and_legacy_branches() {
    let f = Fixture::new();
    for name in ["feat/login", "fix/bug"] {
        f.ok(&["new", "--no-open", name]);
        f.ok(&["new", "--no-open", name]);
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(f.repo.join(".git/twig/state.json")).unwrap())
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
        f.ok(&["stop", name, "-y"]);
        let _ = f.run(&["open", name]);
        f.ok(&["stop", name, "-y"]);
        f.ok(&["rm", name, "-y"]);
        assert!(!f.root.join(name).exists());
        assert!(f.git(&["branch", "--list", name]).is_empty());
    }
    f.ok(&["new", "--no-open", "legacy"]);
    f.git(&["branch", "-m", "legacy", "feature/legacy"]);
    let path = f.repo.join(".git/twig/state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["legacy"]["branch"] = "feature/legacy".into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(
        f.ok(&["new", "--no-open", "legacy"])
            .contains("feature/legacy")
    );
    f.ok(&["stop", "legacy", "-y"]);
    f.ok(&["rm", "legacy", "-y"]);
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
        f.fail(&["new", "--no-open", name], "名称须");
    }
    f.fail(&["new", "--no-open", "HEAD"], "not a valid branch");
    symlink(f._temp.path(), f.root.join("escape")).unwrap();
    f.fail(&["new", "--no-open", "escape/task"], "不是普通目录");
    fs::create_dir_all(f.root.join("nested/.git")).unwrap();
    f.fail(&["new", "--no-open", "nested/task"], "已有仓库");
    assert!(!f.repo.join(".git/twig/state.json").exists());
    assert!(!f._temp.path().join("task").exists());
}

#[test]
fn delete_ignored_is_explicit_and_still_protects_other_files() {
    let f = Fixture::new();
    f.ok(&["new", "--no-open", "cache-task"]);
    let path = f.root.join("cache-task");
    fs::write(f.repo.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(path.join("cache")).unwrap();
    fs::write(path.join("cache/data"), "ignored data").unwrap();
    f.fail(&["rm", "cache-task", "-y"], "被忽略");
    fs::write(path.join("unsaved"), "save me").unwrap();
    f.fail(&["rm", "cache-task", "--delete-ignored", "-y"], "未提交");
    success(
        Command::new("git")
            .current_dir(&path)
            .args(["add", "unsaved"])
            .output()
            .unwrap(),
    );
    f.fail(&["rm", "cache-task", "--delete-ignored", "-y"], "未提交");
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
            "cache-task",
            "--delete-ignored",
            "--expected-ignored",
            "[]",
            "-y",
        ],
        "清单已变化",
    );
    assert!(path.join("cache/data").exists());
    f.ok(&["rm", "cache-task", "--delete-ignored", "-y"]);
    assert!(!path.exists());
}
