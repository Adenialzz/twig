mod support;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn run(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_twig"))
        .arg("--config")
        .arg(root.join("config.toml"))
        .args(args)
        .env("PATH", root)
        .env_remove("COMPLETE")
        .current_dir(root)
        .output()
        .unwrap()
}

#[test]
fn status_is_readonly_and_missing_daemon_never_starts_it() {
    let root = tempfile::tempdir().unwrap();
    let status = run(root.path(), &["agent", "status"]);
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("Twig 后台不可用"));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    executable(
        &root.path().join("codex"),
        "#!/bin/sh\nif [ \"$1 $2 $3\" = 'app-server daemon version' ]; then\nprintf '%s\\n' '{\"status\":\"stopped\"}'\nelse\nprintf '%s\\n' unexpected-start > unexpected\nexit 1\nfi\n",
    );
    let _daemons = support::Daemons(root.path().to_owned());
    let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
    command.env("PATH", root.path()).current_dir(root.path());
    support::daemon::ensure(&command, &root.path().join("config.toml"));
    let init = run(root.path(), &["agent", "init"]);
    assert!(!init.status.success());
    assert!(String::from_utf8_lossy(&init.stderr).contains("请自行运行"));
    assert!(!root.path().join("unexpected").exists());
    assert!(!root.path().join("controller/workspace").exists());
    assert!(!root.path().join("controller/runtime.json").exists());
}

#[test]
fn target_requires_both_project_and_task() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["agent", "send", "--project", "demo", "--message", "x"],
        vec!["agent", "read", "--task", "demo"],
        vec!["agent", "send", "--from-project", "demo", "--message", "x"],
        vec!["agent", "send", "--from-task", "demo", "--message", "x"],
    ] {
        assert_eq!(run(root.path(), &args).status.code(), Some(2));
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn webhook_uses_stdin_for_secret_and_checks_business_status() {
    let root = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\nprintf '%s\\n' \"$@\" > args\n/bin/cat > config-input\nprintf '%s\\n' '{\"code\":0}'\n";
    executable(&root.path().join("curl"), script);
    let url = "https://open.feishu.cn/open-apis/bot/v2/hook/test-secret";
    let _daemons = support::Daemons(root.path().to_owned());
    let call = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_twig"));
        command
            .arg("--config")
            .arg(root.path().join("config.toml"))
            .args(["agent", "notify", "--message", "重要通知"])
            .env("PATH", root.path())
            .env_remove("COMPLETE")
            .env("TWIG_FEISHU_WEBHOOK_URL", url)
            .current_dir(root.path());
        support::daemon::ensure(&command, &root.path().join("config.toml"));
        command.output().unwrap()
    };
    let result = call();
    assert!(result.status.success(), "{:?}", result);
    assert!(
        fs::read_to_string(root.path().join("config-input"))
            .unwrap()
            .contains(url)
    );
    assert!(
        !fs::read_to_string(root.path().join("args"))
            .unwrap()
            .contains("test-secret")
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains("test-secret"));
    executable(
        &root.path().join("curl"),
        &script.replace("\"code\":0", "\"code\":19024"),
    );
    let result = call();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("19024"));
    assert!(!String::from_utf8_lossy(&result.stderr).contains("test-secret"));
}
