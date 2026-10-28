use std::{fs, os::unix::fs::PermissionsExt, process::Command};

fn script(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn install_restart_uses_installed_binary_and_never_restarts_after_failed_install() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&home).unwrap();
    let log = dir.path().join("calls");
    let twig = dir.path().join("mock-twig");
    script(
        &twig,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$TEST_CALLS\"\n",
    );
    script(
        &bin.join("cargo"),
        "#!/bin/sh\nprintf '%s\\n' install >> \"$TEST_CALLS\"\nif [ \"$TEST_FAIL\" = 1 ]; then exit 1; fi\nmkdir -p \"$HOME/.cargo/bin\"\ncp \"$TEST_TWIG\" \"$HOME/.cargo/bin/twig\"\n",
    );
    let run = |target: &str, fail: bool| {
        Command::new("make")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([target, "CONFIG=/tmp/config with spaces.toml"])
            .env("HOME", &home)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env_remove("CARGO_HOME")
            .env_remove("CARGO_INSTALL_ROOT")
            .env_remove("TWIG_BIN")
            .env("TEST_CALLS", &log)
            .env("TEST_TWIG", &twig)
            .env("TEST_FAIL", if fail { "1" } else { "0" })
            .output()
            .unwrap()
    };
    assert!(run("install", false).status.success());
    assert_eq!(fs::read_to_string(&log).unwrap(), "install\n");
    fs::write(&log, "").unwrap();
    let result = run("install-restart", false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "install\n--config\n/tmp/config with spaces.toml\ndaemon\nrestart\n"
    );
    assert!(
        home.join(".twig/controller/workspace/.agents/skills/twig-controller/SKILL.md")
            .exists()
    );
    assert!(!home.join(".codex").exists());
    fs::write(&log, "").unwrap();
    assert!(!run("install-restart", true).status.success());
    assert_eq!(fs::read_to_string(&log).unwrap(), "install\n");
}
