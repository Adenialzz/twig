use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Daemon {
    temp: TempDir,
    child: std::process::Child,
}
impl Daemon {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        command
            .arg("--config")
            .arg(self.temp.path().join("config.toml"));
        command
    }
    fn start() -> Self {
        Self::seeded(None)
    }
    fn seeded(seed: Option<serde_json::Value>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        if let Some(seed) = seed {
            std::fs::write(temp.path().join("notifications.json"), seed.to_string()).unwrap();
            std::fs::create_dir(temp.path().join("controller")).unwrap();
            std::fs::write(temp.path().join("controller/runtime.json"), serde_json::json!({"socket":null,"bindings":{"controller":{"thread":"controller-thread","cwd":temp.path(),"project":null,"task":null,"stopped":true}},"messages":[],"seen":[]}).to_string()).unwrap();
        }
        let child = Command::new(env!("CARGO_BIN_EXE_twig"))
            .arg("--config")
            .arg(temp.path().join("config.toml"))
            .env("PATH", "")
            .args(["daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut daemon = Self { temp, child };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = daemon
                .command()
                .args(["daemon", "status"])
                .output()
                .unwrap();
            if result.status.success()
                && String::from_utf8_lossy(&result.stdout).contains("instance")
            {
                break;
            }
            assert!(daemon.child.try_wait().unwrap().is_none(), "后台提前退出");
            assert!(Instant::now() < deadline, "后台未就绪");
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.command().args(["daemon", "stop"]).output();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
#[test]
fn lifecycle_singleton_and_config_isolation() {
    let mut daemon = Daemon::start();
    let status = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["pid"], daemon.child.id());
    let duplicate = daemon.command().args(["daemon", "run"]).output().unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("已运行"));
    let other = Command::new(env!("CARGO_BIN_EXE_twig"))
        .arg("--config")
        .arg(daemon.temp.path().join("other.toml"))
        .args(["daemon", "status"])
        .output()
        .unwrap();
    assert!(other.status.success());
    assert!(String::from_utf8_lossy(&other.stdout).contains("未运行"));
    assert!(!daemon.temp.path().join("other.toml").exists());
    let stop = daemon.command().args(["daemon", "stop"]).output().unwrap();
    assert!(stop.status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while daemon.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "后台停止超时");
        thread::sleep(Duration::from_millis(20));
    }
    let mut errors = String::new();
    daemon
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut errors)
        .unwrap();
    assert!(errors.is_empty(), "{errors}");
    assert!(!daemon.temp.path().join("config.toml").exists());
}

fn rpc(daemon: &Daemon, method: &str, params: serde_json::Value) -> serde_json::Value {
    let reply = rpc_response(daemon, method, params);
    assert!(reply["error"].is_null(), "{reply}");
    reply["result"].clone()
}

fn rpc_response(daemon: &Daemon, method: &str, params: serde_json::Value) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    let status = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let mut socket =
        std::os::unix::net::UnixStream::connect(status["socket"].as_str().unwrap()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        serde_json::json!({"version":status["version"],"id":"test","method":method,"params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
#[test]
fn two_clients_share_unread_and_refresh_snapshots_across_instances() {
    use serde_json::json;
    let daemon = Daemon::seeded(Some(
        json!({"unread":[{"id":"event","thread":"controller-thread","task":null}],"seen":["event"],"deliveries":{"seen":[],"initialized":false},"watched":{}}),
    ));
    let one = rpc(&daemon, "inbox", json!("one"));
    let two = rpc(&daemon, "inbox", json!("two"));
    assert_eq!(one["unread"], two["unread"]);
    assert_eq!(one["unread"].as_array().unwrap().len(), 1);
    rpc(
        &daemon,
        "viewing",
        json!({"client":"one","target":"controller","seen":["event"]}),
    );
    assert!(
        rpc(&daemon, "inbox", json!("two"))["unread"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let request = json!({"project":null,"path":null,"history":false,"descendants":false,"page":0,"turn":null,"refresh":0,"background":true});
    let first = rpc(
        &daemon,
        "snapshot",
        json!({"client":"one","request":request}),
    );
    let second = rpc(
        &daemon,
        "snapshot",
        json!({"client":"two","request":request,"version":first["version"],"instance":"previous-process"}),
    );
    assert!(
        !second["snapshot"].is_null(),
        "重启后相同版本号也必须重新返回快照"
    );
    let status = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["collection"]["clients"], 2);
    rpc(&daemon, "disconnect", json!("one"));
    rpc(&daemon, "disconnect", json!("two"));
    assert!(
        rpc(&daemon, "inbox", json!("reopened"))["unread"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn management_protocol_stays_stable_while_business_versions_are_rejected() {
    use std::io::{BufRead, BufReader, Write};
    let mut daemon = Daemon::start();
    let status = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    for method in ["status", "operation.submit", "stop"] {
        let mut stream =
            std::os::unix::net::UnixStream::connect(status["socket"].as_str().unwrap()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"version":1,"id":"old-client","method":method,"params":null})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if method == "operation.submit" {
            assert!(response["error"].as_str().unwrap().contains("协议不兼容"));
        } else {
            assert!(response["error"].is_null(), "{response}");
            assert_eq!(response["version"], 1);
            if method == "status" {
                assert_eq!(response["result"]["version"], status["version"]);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while daemon.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "管理协议 stop 未生效");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn restart_refuses_foreground_and_missing_daemons_without_starting_another() {
    let daemon = Daemon::start();
    let before = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let before: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    let result = daemon
        .command()
        .args(["daemon", "restart"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("服务管理器"));
    let after = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let after: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(before["instance"], after["instance"]);
    assert_eq!(after["stopping"], false);
    let absent = daemon.temp.path().join("absent/config.toml");
    let result = Command::new(env!("CARGO_BIN_EXE_twig"))
        .arg("--config")
        .arg(&absent)
        .args(["daemon", "restart"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!absent.parent().unwrap().exists());
}

#[test]
fn restart_loads_installed_executable_and_preserves_original_environment_and_state() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("original bin");
    fs::create_dir(&bin).unwrap();
    let capture = temp.path().join("environment");
    fs::write(bin.join("codex"), "#!/bin/sh\nprintf '%s\\n' \"$PATH\" \"$SHELL\" \"$CODEX_HOME\" \"$TWIG_FEISHU_WEBHOOK_URL\" > \"$TWIG_TEST_CAPTURE\"\nprintf '%s\\n' '{\"status\":\"stopped\"}'\n").unwrap();
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o700)).unwrap();
    let executable = temp.path().join("installed twig");
    fs::copy(env!("CARGO_BIN_EXE_twig"), &executable).unwrap();
    let child = Command::new(&executable)
        .arg("--config")
        .arg(temp.path().join("config.toml"))
        .args(["daemon", "run", "--detached"])
        .env("PATH", &bin)
        .env("SHELL", "/original/shell")
        .env("CODEX_HOME", temp.path().join("original-codex"))
        .env("TWIG_FEISHU_WEBHOOK_URL", "test-secret-not-real")
        .env_remove("TWIG_FEISHU_APP_ID")
        .env_remove("TWIG_FEISHU_APP_SECRET")
        .env_remove("TWIG_FEISHU_ALLOWED_USERS")
        .env("TWIG_TEST_CAPTURE", &capture)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon { temp, child };
    let deadline = Instant::now() + Duration::from_secs(5);
    let before: serde_json::Value = loop {
        let out = daemon
            .command()
            .args(["daemon", "status"])
            .output()
            .unwrap();
        if let Ok(value) = serde_json::from_slice(&out.stdout) {
            break value;
        }
        assert!(daemon.child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    };
    // 拒绝过期实例或不可执行目标时，原后台必须继续运行。
    for (previous, target) in [
        (serde_json::json!("stale-instance"), executable.clone()),
        (
            before["instance"].clone(),
            daemon.temp.path().join("missing-binary"),
        ),
    ] {
        let refused = rpc_response(
            &daemon,
            "restart",
            serde_json::json!({
                "previous":previous,"id":"invalid-restart","executable":target
            }),
        );
        assert!(refused["error"].is_string());
        let status = daemon
            .command()
            .args(["daemon", "status"])
            .output()
            .unwrap();
        let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(status["instance"], before["instance"]);
        assert_eq!(status["stopping"], false);
    }
    // 模拟 cargo install 原子替换正在运行的可执行文件；新版由调用者路径指定。
    let replacement = daemon.temp.path().join("replacement");
    fs::copy(env!("CARGO_BIN_EXE_twig"), &replacement).unwrap();
    fs::rename(&replacement, &executable).unwrap();
    fs::write(daemon.temp.path().join("config.toml"), "[projects]\n").unwrap();
    let original_config = fs::read(daemon.temp.path().join("config.toml")).unwrap();
    let output = Command::new(&executable)
        .arg("--config")
        .arg(daemon.temp.path().join("config.toml"))
        .args(["daemon", "restart"])
        .env("PATH", "")
        .env("SHELL", "/wrong/shell")
        .env("CODEX_HOME", "/wrong/home")
        .env_remove("TWIG_FEISHU_WEBHOOK_URL")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test-secret"));
    let status = daemon
        .command()
        .args(["daemon", "status"])
        .output()
        .unwrap();
    let after: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_ne!(before["instance"], after["instance"]);
    assert_eq!(
        before["pid"], after["pid"],
        "应 exec 新版本并保持原进程归属"
    );
    assert_eq!(after["detached"], true);
    assert_eq!(
        fs::read(daemon.temp.path().join("config.toml")).unwrap(),
        original_config
    );
    let _ = fs::remove_file(&capture);
    let _ = daemon.command().args(["agent", "init"]).output().unwrap();
    let values = fs::read_to_string(&capture).unwrap();
    assert_eq!(
        values.lines().collect::<Vec<_>>(),
        vec![
            bin.to_str().unwrap(),
            "/original/shell",
            daemon.temp.path().join("original-codex").to_str().unwrap(),
            "test-secret-not-real"
        ]
    );
    // 同一入口可连续重启，不会因上次 restart-id 或旧环境丢失而失败。
    assert!(
        daemon
            .command()
            .args(["daemon", "restart"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
