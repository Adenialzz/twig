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
        let temp = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_twig"))
            .arg("--config")
            .arg(temp.path().join("config.toml"))
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
