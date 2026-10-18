use std::{fs, path::Path, process::Command};
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn complete(cwd: &Path, args: &[&str]) -> (String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_twig"))
        .current_dir(cwd)
        .env("COMPLETE", "zsh")
        .env("_CLAP_COMPLETE_INDEX", args.len().to_string())
        .args(["--", "twig"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn values(cwd: &Path, args: &[&str]) -> String {
    let (out, err) = complete(cwd, args);
    assert!(err.is_empty(), "{err}");
    out
}

#[test]
fn commands_options_and_paths_without_a_repository() {
    let temp = TempDir::new().unwrap();
    assert!(values(temp.path(), &["op"]).starts_with("open:"));
    assert_eq!(values(temp.path(), &["rm", "--keep"]), "--keep-branch");
    assert!(values(temp.path(), &["config", "remote", "d"]).starts_with("default:"));
    fs::create_dir(temp.path().join("work trees")).unwrap();
    fs::write(temp.path().join("settings.toml"), "").unwrap();
    assert!(values(temp.path(), &["init", "--root", "wo"]).contains("work trees/"));
    assert!(values(temp.path(), &["--config", "set"]).contains("settings.toml"));
    let (out, err) = complete(temp.path(), &["open", "missing-task"]);
    assert!(out.is_empty());
    assert!(err.contains("not a git repository"));
    assert!(!temp.path().join(".git").exists());
}

#[test]
fn dynamic_values_follow_config_repo_and_shared_worktree_state() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo with ' spaces");
    let worktree = temp.path().join("linked worktree");
    let config = temp.path().join("custom config.toml");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    git(
        &repo,
        &[
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
        ],
    );
    git(&repo, &["branch", "develop"]);
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/remote-only", "HEAD"],
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/auth",
            worktree.to_str().unwrap(),
        ],
    );
    let state_dir = repo.join(".git/twig");
    fs::create_dir(&state_dir).unwrap();
    let state =
        serde_json::json!({"auth": {"branch": "feature/auth", "base": "main", "path": worktree}})
            .to_string();
    fs::write(state_dir.join("state.json"), &state).unwrap();
    // 已有锁不应阻止只读补全，也不能被补全移除。
    fs::write(state_dir.join("lock"), "existing lock").unwrap();
    fs::write(&config, format!("[projects.demo]\npath = {:?}\nmain = 'main'\ndefault_remote = 'origin'\n[projects.demo.remotes]\norigin = 'local-origin'\nupstream = 'local-upstream'\n", repo.to_str().unwrap())).unwrap();
    let cfg = config.to_str().unwrap();
    for args in [
        vec!["--config", cfg, "--repo", "de"],
        vec!["--config", cfg, "clone", "de"],
        vec!["config", "show", "--config", cfg, "de"],
        vec!["--config", cfg, "config", "remove", "de"],
        vec!["--config", cfg, "config", "remote", "add", "de"],
    ] {
        assert_eq!(values(temp.path(), &args), "demo");
    }
    for action in ["new", "open", "stop", "rm"] {
        assert_eq!(
            values(
                temp.path(),
                &["--config", cfg, "--repo", "demo", action, "a"]
            ),
            "auth"
        );
        assert_eq!(values(&worktree, &[action, "a"]), "auth");
    }
    assert_eq!(
        values(temp.path(), &["open", "--config", cfg, "--repo=demo", "a"]),
        "auth"
    );
    assert_eq!(
        values(
            temp.path(),
            &[&format!("--config={cfg}"), "--repo=demo", "open", "a"]
        ),
        "auth"
    );
    assert_eq!(
        values(
            temp.path(),
            &[
                "--config", cfg, "--repo", "demo", "new", "next", "--base", "dev"
            ]
        ),
        "develop"
    );
    assert_eq!(
        values(&repo, &["rm", "auth", "--target=dev"]),
        "--target=develop"
    );
    assert_eq!(values(&repo, &["init", "--base", "remote"]), "");
    for action in ["remove", "default"] {
        assert_eq!(
            values(
                temp.path(),
                &["--config", cfg, "config", "remote", action, "demo", "up"]
            ),
            "upstream"
        );
    }
    assert_eq!(
        fs::read_to_string(state_dir.join("state.json")).unwrap(),
        state
    );
    assert_eq!(
        fs::read_to_string(state_dir.join("lock")).unwrap(),
        "existing lock"
    );
    assert!(!state_dir.join("config.json").exists());
    assert!(!config.with_extension("lock").exists());
    assert!(worktree.exists());
}

#[test]
fn missing_or_invalid_registry_is_never_created_or_overwritten() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join("missing/config.toml");
    let args = ["--config", config.to_str().unwrap(), "--repo", ""];
    assert_eq!(values(temp.path(), &args), "");
    assert!(!config.parent().unwrap().exists());
    fs::create_dir(config.parent().unwrap()).unwrap();
    fs::write(&config, "invalid TOML {").unwrap();
    let (out, err) = complete(temp.path(), &args);
    assert!(out.is_empty());
    assert!(err.contains("配置格式错误"), "{err}");
    assert_eq!(fs::read_to_string(config).unwrap(), "invalid TOML {");
}

#[test]
fn zsh_registration_loads_and_invokes_the_generated_function() {
    let temp = TempDir::new().unwrap();
    let binary = temp.path().join("twig with ' spaces");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_twig"), &binary).unwrap();
    let output = Command::new("zsh")
        .args([
            "-fc",
            r#"
autoload -Uz compinit
compinit -D
source <(COMPLETE=zsh "$1")
words=(twig config remote d)
CURRENT=4
_describe() { print -rl -- "${(@P)3}"; }
_twig_complete
"#,
            "test",
            binary.to_str().unwrap(),
        ])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("default:"));
}

#[test]
fn zsh_tab_orders_tasks_and_handles_quoted_paths() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join("work trees/nested")).unwrap();
    fs::create_dir_all(temp.path().join("foo:bar/nested")).unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    let state_dir = temp.path().join(".git/twig");
    fs::create_dir(&state_dir).unwrap();
    let feature = serde_json::json!({"branch": "main", "base": "main", "path": temp.path()});
    fs::write(
        state_dir.join("state.json"),
        serde_json::json!({"alpha": feature, "beta": feature}).to_string(),
    )
    .unwrap();
    let registry = format!(
        "[projects.demo]\npath = {:?}\nmain = 'main'\ndefault_remote = 'origin'\n[projects.demo.remotes]\norigin = 'local'\n",
        temp.path().to_str().unwrap()
    );
    fs::write(temp.path().join("custom config.toml"), registry).unwrap();
    // zpty 驱动真实 ZLE/Tab，不替换 _describe；用 widget 读取最终选中的输入。
    let output = Command::new("zsh")
        .args(["-fc", r#"
zmodload zsh/zpty
zmodload zsh/zselect
trap '(( $? )) && print -ru2 -- "$transcript"; zpty -d' EXIT
wait_for() {
    local chunk
    transcript=''
    for attempt in {1..500}; do
        while zpty -r shell chunk; do
            transcript+=$chunk
        done
        [[ $transcript == *"$1"* ]] && return 0
        zselect -t 1
    done
    print -ru2 -- "等待 $1 超时：$transcript"
    return 1
}
start_shell() {
    zpty -d
    zpty -b shell zsh -f
    zpty -w shell 'autoload -Uz compinit; compinit -D; source <(COMPLETE=zsh "$TWIG_TEST_BIN"); _dump_buffer() { print -r -- "TWIG_BUFFER=$BUFFER"; zle redisplay; }; zle -N _dump_buffer; bindkey "^Xq" _dump_buffer; PROMPT="READY""> "'
    wait_for 'READY> ' || exit 1
}
for action in open new stop rm; do
    start_shell
    zpty -w -n shell "twig $action "$'\t\t\x18q'
    wait_for "TWIG_BUFFER=twig $action alpha" || exit 1
    # 在列表里 alpha 先于选项出现，说明文字也仍保留。
    [[ $transcript == *alpha*beta*--repo*'在任意目录操作已登记项目'* ]] || {
        print -ru2 -- "$transcript"
        exit 1
    }
done
start_shell
zpty -w -n shell $'twig open b\t\x18q'
wait_for 'TWIG_BUFFER=twig open beta ' || exit 1
[[ $transcript != *--repo* ]] || exit 1
start_shell
zpty -w -n shell $'twig open --\t\t\x18q'
wait_for 'TWIG_BUFFER=twig open --repo' || exit 1
[[ $transcript != *alpha* && $transcript != *beta* ]] || exit 1
start_shell
zpty -w -n shell $'twig init --root wo\t\x18q'
wait_for 'TWIG_BUFFER=twig init --root work\ trees/' || exit 1
for option in '--root ' '--root='; do
    for candidate_path in 'work\ trees' 'foo:bar'; do
        start_shell
        # 连按两次补全：先补目录，再输入子目录前缀，验证没有多余空格。
        prefix=${candidate_path[1,2]}
        zpty -w -n shell "twig init $option$prefix"$'\tne\t\x18q'
        wait_for "TWIG_BUFFER=twig init $option$candidate_path/nested/" || exit 1
    done
done
for option in '--config ' '--config='; do
    for config in "'custom config.toml'" '"custom config.toml"' 'custom\ config.toml'; do
        start_shell
        zpty -w -n shell "twig $option$config --repo demo open a"$'\t\x18q'
        wait_for "TWIG_BUFFER=twig $option$config --repo demo open alpha " || exit 1
    done
done
"#])
        .current_dir(temp.path())
        .env("TWIG_TEST_BIN", env!("CARGO_BIN_EXE_twig"))
        .env("TERM", "xterm")
        .env("COLUMNS", "120")
        .env("LINES", "40")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
