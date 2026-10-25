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
        serde_json::json!({"version":1,"id":"test","method":method,"params":params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(reply["error"].is_null(), "{reply}");
    reply["result"].clone()
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
        json!({"client":"one","target":"controller"}),
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
