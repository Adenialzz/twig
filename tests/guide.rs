use std::{fs, path::Path, process::Command};

fn read(args: &[&str]) -> std::process::Output {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("broken.toml");
    fs::write(&config, "invalid toml [").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_twig"))
        .arg("--config")
        .arg(&config)
        .args(args)
        .current_dir(temp.path())
        .env("HOME", temp.path().join("absent-home"))
        .env("PATH", "")
        .env_remove("COMPLETE")
        .output()
        .unwrap();
    assert_eq!(fs::read_to_string(config).unwrap(), "invalid toml [");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    output
}

#[test]
fn offline_manual_covers_every_document_without_accessing_resources() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let index = read(&["guide"]);
    assert!(index.status.success());
    assert!(index.stderr.is_empty());
    assert_eq!(index.stdout, fs::read(docs.join("index.md")).unwrap());
    let all = read(&["guide", "--all"]);
    assert!(all.status.success());
    let all = String::from_utf8(all.stdout).unwrap();
    for entry in fs::read_dir(docs).unwrap() {
        let path = entry.unwrap().path();
        let topic = path.file_stem().unwrap().to_str().unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(all.contains(&content), "缺少主题：{topic}");
        if topic != "index" {
            let output = read(&["guide", topic]);
            assert!(output.status.success(), "{topic}");
            assert!(output.stderr.is_empty());
            assert_eq!(output.stdout, content.as_bytes());
            assert!(
                String::from_utf8_lossy(&index.stdout).contains(&format!("twig guide {topic}"))
            );
        }
    }
}

#[test]
fn help_discovers_manual_and_guide_rejects_invalid_arguments() {
    for command in [
        "add", "new", "open", "ls", "stop", "rm", "pr", "console", "guide",
    ] {
        let output = read(&[command, "--help"]);
        assert!(output.status.success(), "{command}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("twig guide"));
    }
    let output = read(&["--help"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("twig guide"));
    for args in [vec!["guide", "unknown"], vec!["guide", "windows", "--all"]] {
        let output = read(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
