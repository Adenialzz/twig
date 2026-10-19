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
    f.ok(&["add", "local", "--path", f.repo.to_str().unwrap()]);
    f.ok(&["new", "local", "--no-open", "auth"]);
    f.ok(&["new", "local", "--no-open", "auth"]);
    f.ok(&["new", "local", "--no-open", "search"]);
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
    f.ok(&["new", "local", "--no-open", "auth"]);
    // open 的非交互 attach 会失败，但必须已经重建窗口。
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
    f.ok(&["new", "local", "--no-open", "search"]);
}

#[test]
fn refuses_dirty_unmerged_and_busy_removal() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-open", "auth"]);
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
fn detects_conflicts_and_resumes_partial_creation() {
    let f = Fixture::new();
    f.git(&["branch", "existing"]);
    f.fail(&["new", "local", "--no-open", "existing"], "分支已存在");
    fs::create_dir(f.root.join("occupied")).unwrap();
    f.fail(&["new", "local", "--no-open", "occupied"], "路径已存在");
    f.fail(&["new", "local", "--no-open", "../escape"], "名称须");
    let out = f
        .command(&f.repo)
        .env("TWIG_FAIL_TMUX", "1")
        .args(["new", "local", "--no-open", "partial"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(f.root.join("partial").is_dir());
    f.ok(&["new", "local", "--no-open", "partial"]);
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
    f.ok(&["new", "local", "--no-open", "ignored"]);
    fs::write(f.repo.join(".git/info/exclude"), "cache/\n").unwrap();
    fs::create_dir(f.root.join("ignored/cache")).unwrap();
    fs::write(f.root.join("ignored/cache/file"), "data").unwrap();
    f.fail(&["rm", "local", "ignored", "-y"], "被忽略");
    f.ok(&["new", "local", "--no-open", "lost"]);
    f.ok(&["stop", "local", "lost", "-y"]);
    fs::remove_dir_all(f.root.join("lost")).unwrap();
    f.fail(&["new", "local", "--no-open", "lost"], "目录丢失");
    f.git(&["worktree", "prune"]);
    f.ok(&["new", "local", "--no-open", "lost"]);
}

#[test]
fn merged_branch_cleanup_and_confirmation() {
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-open", "done"]);
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
    f.ok(&["new", "local", "--no-open", "0"]);
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
    f.ok(&["new", "local", "--no-open", "0"]);
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
    f.ok(&["new", "local", "--no-open", "unpushed"]);
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
    f.fail(&["new", "local", "--no-open", "foreign"], "同名非 twig");
    assert!(f.root.join("foreign").is_dir());
    f.mux(&[
        "rename-window",
        "-t",
        &format!("{}:foreign", f.session()),
        "personal",
    ]);
    f.ok(&["new", "local", "--no-open", "foreign"]);
}

#[test]
fn rechecks_files_after_confirmation_prompt() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-open", "race"]);
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
fn new_and_open_switch_an_attached_client_between_sessions() {
    use std::process::Stdio;
    let f = Fixture::new();
    f.ok(&["new", "local", "--no-open", "auth"]);
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
    for action in ["open local auth", "new local fresh", "new local fresh"] {
        f.mux(&["switch-client", "-t", "driver"]);
        wait_for("driver");
        let command = format!(
            "'{}' --config '{}' {action}",
            env!("CARGO_BIN_EXE_twig").replace('\'', "'\\''"),
            f.registry_path().to_str().unwrap().replace('\'', "'\\''")
        );
        f.mux(&["send-keys", "-t", "driver:", &command, "Enter"]);
        wait_for(&f.session());
        let expected = if action == "open local auth" {
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
        assert!(
            !support::project_dir(&f.repo, &f.registry_path())
                .join("lock")
                .exists()
        );
    }
    f.ok(&["new", "local", "--no-open", "background"]);
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
    f.fail(&["new", "local", "auth"], "tmux 进入失败；环境已保留");
    assert!(f.root.join("auth/.git").exists());
    assert!(
        !support::project_dir(&f.repo, &f.registry_path())
            .join("lock")
            .exists()
    );
    f.ok(&["new", "local", "--no-open", "auth"]);
    assert!(f.ok(&["ls", "local"]).contains("Open"));
}

#[test]
fn slash_task_names_and_legacy_branches() {
    let f = Fixture::new();
    for name in ["feat/login", "fix/bug"] {
        f.ok(&["new", "local", "--no-open", name]);
        f.ok(&["new", "local", "--no-open", name]);
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
    f.ok(&["new", "local", "--no-open", "legacy"]);
    f.git(&["branch", "-m", "legacy", "feature/legacy"]);
    let path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["legacy"]["branch"] = "feature/legacy".into();
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(
        f.ok(&["new", "local", "--no-open", "legacy"])
            .contains("feature/legacy")
    );
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
        f.fail(&["new", "local", "--no-open", name], "名称须");
    }
    f.fail(&["new", "local", "--no-open", "HEAD"], "not a valid branch");
    symlink(f._temp.path(), f.root.join("escape")).unwrap();
    f.fail(
        &["new", "local", "--no-open", "escape/task"],
        "不是普通目录",
    );
    fs::create_dir_all(f.root.join("nested/.git")).unwrap();
    f.fail(&["new", "local", "--no-open", "nested/task"], "已有仓库");
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
    f.ok(&["new", "local", "--no-open", "cache-task"]);
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
    f.ok(&["new", "local", "shared", "--no-open"]);
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
    success(f.registered(&["new", "local", "auth", "--no-open"]));
    assert!(success(f.registered(&["ls"])).contains("local"));
    assert!(success(f.registered(&["ls", "local"])).contains("auth"));
    let state_path = support::project_dir(&f.repo, &f.registry_path()).join("state.json");
    let before = fs::read(&state_path).unwrap();
    let registry = fs::read(f.registry_path()).unwrap();
    for args in [
        vec!["new", "local"],
        vec!["open", "local"],
        vec!["stop", "local"],
        vec!["pr", "local"],
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
    f.ok(&["new", "local", "auth", "--no-open"]);
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
    success(f.registered(&["new", "other", "task", "--no-open"]));
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
    assert!(!registry.parent().unwrap().exists());
}
