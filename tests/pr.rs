mod support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
    task: PathBuf,
    bare: PathBuf,
    bin: PathBuf,
    real_git: PathBuf,
    log: PathBuf,
    responses: PathBuf,
    body: PathBuf,
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn script(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    fn new(url: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let real_git = PathBuf::from(
            success(
                Command::new("sh")
                    .args(["-c", "command -v git"])
                    .output()
                    .unwrap(),
            )
            .trim(),
        );
        let f = Self {
            repo: root.join("repo"),
            task: root.join("task with ' spaces"),
            bare: root.join("remote.git"),
            bin: root.join("bin"),
            log: root.join("calls"),
            responses: root.join("responses.json"),
            body: root.join("body.md"),
            _temp: temp,
            real_git,
        };
        fs::create_dir(&f.repo).unwrap();
        fs::create_dir(&f.bin).unwrap();
        fs::write(&f.responses, "[]").unwrap();
        fs::write(&f.log, "").unwrap();
        f.git(&f.repo, &["init", "-b", "main"]);
        f.git(&f.repo, &["config", "user.name", "Test"]);
        f.git(&f.repo, &["config", "user.email", "test@example.invalid"]);
        f.git(&f.repo, &["config", "commit.gpgsign", "false"]);
        f.git(&f.repo, &["commit", "--allow-empty", "-m", "initial"]);
        f.git(&f.repo, &["init", "--bare", f.bare.to_str().unwrap()]);
        f.git(&f.repo, &["push", f.bare.to_str().unwrap(), "main:main"]);
        f.git(&f.repo, &["remote", "add", "origin", url]);
        f.git(
            &f.repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature/task",
                f.task.to_str().unwrap(),
            ],
        );
        f.git(
            &f.task,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "新增测试功能",
                "-m",
                "第一行\n第二行",
            ],
        );
        fs::write(
            f._temp.path().join("registry.toml"),
            format!("[projects.demo]\npath = {:?}\n", f.repo.to_str().unwrap()),
        )
        .unwrap();
        let state_dir = support::project_dir(&f.repo, &f._temp.path().join("registry.toml"));
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("config.json"),
            serde_json::json!({"version": 1, "main": f.repo,
            "root": root, "base": "main", "target": "main", "session": "unused"})
            .to_string(),
        )
        .unwrap();
        fs::write(
            state_dir.join("state.json"),
            serde_json::json!({"task": {"branch": "feature/task", "base": "main", "path": f.task}})
                .to_string(),
        )
        .unwrap();
        // 仅替换联网的 Git 子命令，实际 push/ls-remote 在本地 bare 仓库执行。
        script(
            &f.bin.join("git"),
            r#"#!/bin/sh
case "$1" in
    ls-remote)
        printf '<%s>' "$@" >> "$TWIG_TEST_LOG"
        printf '\n' >> "$TWIG_TEST_LOG"
        shift 4
        exec "$TWIG_TEST_GIT" ls-remote --heads -- "$TWIG_TEST_REMOTE" "$@"
        ;;
    push)
        printf '<%s>' "$@" >> "$TWIG_TEST_LOG"
        printf '\n' >> "$TWIG_TEST_LOG"
        exec "$TWIG_TEST_GIT" -c "remote.$5.url=$TWIG_TEST_REMOTE" -c "remote.$5.pushurl=$TWIG_TEST_REMOTE" "$@"
        ;;
    *) exec "$TWIG_TEST_GIT" "$@" ;;
esac
"#,
        );
        for cli in ["gh", "glab"] {
            script(
                &f.bin.join(cli),
                r#"#!/bin/sh
printf '<%s>' "$@" >> "$TWIG_TEST_LOG"
printf '\n' >> "$TWIG_TEST_LOG"
case "$1/$2" in
    --version/) echo 'test-cli';;
    pr/list|mr/list)
        if [ "$TWIG_TEST_LIST_FAIL" = 1 ]; then echo 'authentication failed' >&2; exit 1; fi
        cat "$TWIG_TEST_RESPONSES";;
    pr/create|mr/create)
        previous=''
        for argument in "$@"; do
            case "$previous" in --body-file|--description-file) cp "$argument" "$TWIG_TEST_BODY";; esac
            previous="$argument"
        done
        if [ "$TWIG_TEST_CREATE_FAIL" = 1 ]; then echo 'creation failed' >&2; exit 1; fi
        echo 'https://example.invalid/request/1';;
    *) echo 'unexpected command' >&2; exit 2;;
esac
"#,
            );
        }
        // PR 操作与补全不应调用 tmux。
        script(
            &f.bin.join("tmux"),
            "#!/bin/sh\necho unexpected-tmux >> \"$TWIG_TEST_LOG\"\nexit 1\n",
        );
        f
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        success(
            Command::new(&self.real_git)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(args)
                .output()
                .unwrap(),
        )
        .trim_end()
        .to_owned()
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_twig"));
        c.arg("--config")
            .arg(self._temp.path().join("registry.toml"));
        c.current_dir(cwd)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("TWIG_TEST_LOG", &self.log)
            .env("TWIG_TEST_REMOTE", &self.bare)
            .env("TWIG_TEST_GIT", &self.real_git)
            .env("TWIG_TEST_RESPONSES", &self.responses)
            .env("TWIG_TEST_BODY", &self.body)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(&self.task).args(args).output().unwrap()
    }
    fn calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap()
    }
    fn published(&self, branch: &str) -> String {
        self.git(
            &self.repo,
            &[
                "ls-remote",
                self.bare.to_str().unwrap(),
                &format!("refs/heads/{branch}"),
            ],
        )
    }
    fn publish(&self) {
        self.git(
            &self.task,
            &[
                "push",
                self.bare.to_str().unwrap(),
                "feature/task:feature/task",
            ],
        );
    }
    fn fail(&self, args: &[&str], message: &str) {
        let out = self.run(args);
        assert!(!out.status.success(), "应拒绝 {args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn github_creation_requires_published_head_and_preserves_body() {
    let f = Fixture::new("git@github.com:team/project.git");
    f.fail(&["pr", "demo", "task"], "请先推送");
    assert!(!f.calls().contains("<push>"));
    assert!(!f.calls().contains("<create>"));
    f.publish();
    let body = "第一段\n\n含 `反引号`、$HOME、$(touch should-not-exist)\n";
    fs::write(f.repo.join("description.md"), body).unwrap();
    let output = success(
        f.command(&f.repo)
            .args([
                "pr",
                "demo",
                "task",
                "--title",
                "标题 ' 引号",
                "--body-file",
                "description.md",
            ])
            .output()
            .unwrap(),
    );
    assert!(output.contains("https://example.invalid/request/1"));
    assert_eq!(fs::read_to_string(&f.body).unwrap(), body);
    let calls = f.calls();
    assert!(
        calls.contains(
            "<--repo><github.com/team/project><--head><feature/task><--base><main><--draft>"
        ),
        "{calls}"
    );
    assert!(!calls.contains("<push>"));
    assert!(!calls.contains("unexpected-tmux"));
    assert!(
        !support::project_dir(&f.repo, &f._temp.path().join("registry.toml"))
            .join("lock")
            .exists()
    );
    fs::write(&f.log, "").unwrap();
    f.git(&f.task, &["commit", "--allow-empty", "-m", "not pushed"]);
    f.fail(&["pr", "demo", "task"], "与本地提交不同");
    assert!(!f.calls().contains("<create>"));
    assert!(!f.calls().contains("<push>"));
}

#[test]
fn gitlab_push_and_existing_request_use_the_same_repository() {
    let f = Fixture::new("https://gitlab.com/group/sub/project.git");
    success(f.run(&["pr", "demo", "task", "--push"]));
    assert!(!f.published("feature/task").is_empty());
    assert_eq!(
        f.git(&f.task, &["config", "branch.feature/task.remote"]),
        "origin"
    );
    let calls = f.calls();
    assert!(calls.contains("<push><--no-follow-tags><--set-upstream><--><origin><refs/heads/feature/task:refs/heads/feature/task>"));
    assert!(calls.contains("<mr><create><--repo><https://gitlab.com/group/sub/project><--head><https://gitlab.com/group/sub/project>"));
    assert!(calls.contains("<--draft><--push=false><--yes>"));
    fs::write(&f.responses, r#"[{"web_url":"https://gitlab.com/group/sub/project/-/merge_requests/1","source_project_id":5,"target_project_id":5}]"#).unwrap();
    fs::write(&f.log, "").unwrap();
    let url = success(f.run(&["pr", "demo", "task"]));
    assert!(url.contains("merge_requests/1"));
    assert!(!f.calls().contains("<create>"));
    assert!(!f.calls().contains("<push>"));
    f.git(
        &f.task,
        &["commit", "--allow-empty", "-m", "another change"],
    );
    success(f.run(&["pr", "demo", "task", "--push"]));
    assert!(f.calls().contains("<push>"));
    assert!(!f.calls().contains("<create>"));
}

#[test]
fn remote_selection_upstream_mapping_and_self_hosted_provider() {
    let f = Fixture::new("ssh://git@git.example.test:2222/group/sub/project.git");
    f.fail(&["pr", "demo", "task"], "--provider");
    f.git(
        &f.repo,
        &[
            "remote",
            "add",
            "other",
            "https://github.com/team/other.git",
        ],
    );
    f.fail(
        &["pr", "demo", "task", "--provider", "gitlab"],
        "没有唯一 remote",
    );
    f.git(&f.repo, &["config", "branch.feature/task.remote", "origin"]);
    f.git(
        &f.repo,
        &[
            "config",
            "branch.feature/task.merge",
            "refs/heads/review/task",
        ],
    );
    success(f.run(&["pr", "demo", "task", "--push", "--provider", "gitlab"]));
    assert!(!f.published("review/task").is_empty());
    assert!(f.calls().contains("<--source-branch><review/task>"));
    assert!(
        f.calls()
            .contains("<--repo><https://git.example.test/group/sub/project>")
    );
    fs::write(&f.log, "").unwrap();
    success(f.run(&["pr", "demo", "task", "--push", "--remote", "other"]));
    assert!(f.calls().contains("<--repo><github.com/team/other>"));
    assert!(
        f.calls()
            .contains("<other><refs/heads/feature/task:refs/heads/feature/task>")
    );
}

#[test]
fn conflicts_and_cli_failures_do_not_push_or_create() {
    let f = Fixture::new("https://github.com/team/project.git");
    f.git(
        &f.repo,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:fork/project.git",
        ],
    );
    f.fail(&["pr", "demo", "task", "--push"], "不同仓库");
    assert!(f.calls().is_empty());
    f.git(&f.repo, &["config", "--unset-all", "remote.origin.pushurl"]);
    f.fail(
        &["pr", "demo", "task", "--push", "--target", "feature/task"],
        "不能相同",
    );
    f.fail(
        &["pr", "demo", "task", "--push", "--target", "missing"],
        "缺少目标分支",
    );
    f.fail(
        &["pr", "demo", "task", "--provider", "gitlab"],
        "公共平台不符",
    );
    let out = f
        .command(&f.task)
        .env("TWIG_TEST_LIST_FAIL", "1")
        .args(["pr", "demo", "task", "--push"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!f.calls().contains("<push>"));
    assert!(!f.calls().contains("<create>"));
    fs::write(&f.responses, "invalid JSON").unwrap();
    f.fail(&["pr", "demo", "task", "--push"], "数据无效");
    assert!(!f.calls().contains("<push>"));
}

#[test]
fn existing_fork_is_not_reused_and_create_failure_keeps_pushed_branch() {
    let f = Fixture::new("https://github.com/team/project.git");
    fs::write(
        &f.responses,
        r#"[{"url":"https://example.invalid/fork","isCrossRepository":true}]"#,
    )
    .unwrap();
    let out = f
        .command(&f.task)
        .env("TWIG_TEST_CREATE_FAIL", "1")
        .args(["pr", "demo", "task", "--push"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("已推送的提交会保留"));
    assert!(!f.published("feature/task").is_empty());
    assert!(f.task.exists());
    assert!(
        !support::project_dir(&f.repo, &f._temp.path().join("registry.toml"))
            .join("lock")
            .exists()
    );
}

#[test]
fn task_identity_and_completion_are_local() {
    let f = Fixture::new("https://github.com/team/project.git");
    let out = f.command(&f.repo).args(["pr", "demo"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("required arguments"));
    fs::create_dir(f.task.join("subdir")).unwrap();
    fs::write(
        &f.responses,
        r#"[{"url":"https://example.invalid/existing","isCrossRepository":false}]"#,
    )
    .unwrap();
    assert!(
        success(
            f.command(&f.task.join("subdir"))
                .args(["pr", "demo", "task"])
                .output()
                .unwrap()
        )
        .contains("existing")
    );
    fs::write(&f.log, "").unwrap();
    for (args, expected) in [
        (vec!["pr", "demo", "ta"], "task"),
        (vec!["pr", "demo", "task", "--remote", "or"], "origin"),
        (vec!["pr", "demo", "task", "--provider", "gitl"], "gitlab"),
    ] {
        let out = success(
            f.command(&f.task)
                .env("COMPLETE", "zsh")
                .env("_CLAP_COMPLETE_INDEX", (args.len() + 2).to_string())
                .args(["--", "twig", "--config"])
                .arg(f._temp.path().join("registry.toml"))
                .args(args)
                .output()
                .unwrap(),
        );
        assert_eq!(out, expected);
    }
    assert!(f.calls().is_empty());
    f.git(&f.task, &["switch", "--detach"]);
    f.fail(&["pr", "demo", "task", "--push"], "分支已改变");
    assert!(f.calls().is_empty());
}

#[test]
fn registered_project_target_override_and_missing_cli() {
    let f = Fixture::new("https://github.com/team/project.git");
    f.git(&f.repo, &["push", f.bare.to_str().unwrap(), "main:release"]);
    let config = f._temp.path().join("registry.toml");
    fs::write(
        &config,
        format!("[projects.demo]\npath = {:?}\n", f.repo.to_str().unwrap()),
    )
    .unwrap();
    success(
        f.command(f._temp.path())
            .args(["pr", "demo", "task", "--push", "--target", "release"])
            .output()
            .unwrap(),
    );
    assert!(f.calls().contains("<--base><release>"));
    // 从任务目录调用同样使用显式项目和任务名。
    success(
        f.command(&f.task)
            .args(["pr", "demo", "task"])
            .output()
            .unwrap(),
    );
    fs::remove_file(f.bin.join("gh")).unwrap();
    fs::write(&f.log, "").unwrap();
    let out = f
        .command(&f.task)
        .env("PATH", &f.bin)
        .args(["pr", "demo", "task", "--push"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("先安装并登录"));
    assert!(f.calls().is_empty());
}

#[test]
fn rejected_push_does_not_force_or_create_a_request() {
    let f = Fixture::new("https://github.com/team/project.git");
    f.git(
        &f.task,
        &["commit", "--allow-empty", "-m", "remote-only commit"],
    );
    f.publish();
    let before = f.published("feature/task");
    // 仅在临时仓库构造分叉，不触碰真实项目。
    f.git(&f.task, &["reset", "--hard", "HEAD~1"]);
    f.git(
        &f.task,
        &["commit", "--allow-empty", "-m", "local-only commit"],
    );
    f.fail(&["pr", "demo", "task", "--push"], "推送失败");
    assert_eq!(f.published("feature/task"), before);
    assert!(!f.calls().contains("<create>"));
    assert!(
        !support::project_dir(&f.repo, &f._temp.path().join("registry.toml"))
            .join("lock")
            .exists()
    );
}
