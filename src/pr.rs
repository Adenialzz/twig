use crate::{
    config::{Config, State},
    git::{self, git},
};
use anyhow::{Context, Result, bail, ensure};
use clap::ValueEnum;
use clap_complete::ArgValueCompleter;
use serde::Deserialize;
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(clap::Args)]
pub struct Args {
    #[arg(add = ArgValueCompleter::new(crate::completion::projects))]
    pub project: String,
    /// 任务名
    #[arg(add = ArgValueCompleter::new(crate::completion::tasks))]
    name: String,
    /// 显式推送任务分支，不强制覆盖远端
    #[arg(long)]
    push: bool,
    /// Git remote；默认 upstream remote，否则使用唯一 remote
    #[arg(long, add = ArgValueCompleter::new(crate::completion::git_remotes))]
    remote: Option<String>,
    /// 覆盖项目配置中的合并目标分支
    #[arg(long, add = ArgValueCompleter::new(crate::completion::branches))]
    target: Option<String>,
    /// 自建域名须指定平台
    #[arg(long, value_enum)]
    provider: Option<Provider>,
    /// 草稿标题；默认使用任务最新提交的标题
    #[arg(long)]
    title: Option<String>,
    /// 草稿正文文件；默认使用任务最新提交的完整提交说明
    #[arg(long)]
    body_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Provider {
    Github,
    Gitlab,
}

impl Provider {
    fn program(self) -> &'static str {
        match self {
            Self::Github => "gh",
            Self::Gitlab => "glab",
        }
    }

    fn run(self, cwd: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new(self.program())
            .current_dir(cwd)
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("无法运行 {}；请先安装并登录", self.program()))?;
        ensure!(
            output.status.success(),
            "{} 执行失败：{}",
            self.program(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)?.trim_end().to_owned())
    }

    fn existing(
        self,
        cwd: &Path,
        repo: &str,
        source: &str,
        target: &str,
    ) -> Result<Option<String>> {
        let urls: Vec<String> = match self {
            Self::Github => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Request {
                    url: String,
                    is_cross_repository: bool,
                }
                let json = self.run(
                    cwd,
                    &[
                        "pr",
                        "list",
                        "--repo",
                        repo,
                        "--head",
                        source,
                        "--base",
                        target,
                        "--state",
                        "open",
                        "--limit",
                        "100",
                        "--json",
                        "url,isCrossRepository",
                    ],
                )?;
                serde_json::from_str::<Vec<Request>>(&json)
                    .context("gh 返回的 PR 数据无效")?
                    .into_iter()
                    .filter(|r| !r.is_cross_repository)
                    .map(|r| r.url)
                    .collect()
            }
            Self::Gitlab => {
                #[derive(Deserialize)]
                struct Request {
                    web_url: String,
                    source_project_id: u64,
                    target_project_id: u64,
                }
                let json = self.run(
                    cwd,
                    &[
                        "mr",
                        "list",
                        "--repo",
                        repo,
                        "--source-branch",
                        source,
                        "--target-branch",
                        target,
                        "--per-page",
                        "100",
                        "--output",
                        "json",
                    ],
                )?;
                serde_json::from_str::<Vec<Request>>(&json)
                    .context("glab 返回的 MR 数据无效")?
                    .into_iter()
                    .filter(|r| r.source_project_id == r.target_project_id)
                    .map(|r| r.web_url)
                    .collect()
            }
        };
        ensure!(urls.len() <= 1, "找到多个匹配的 PR/MR，请使用官方 CLI 检查");
        Ok(urls.into_iter().next())
    }
}

#[derive(Debug)]
struct Repository {
    host: String,
    path: String,
    scheme: String,
}

impl Repository {
    fn parse(url: &str) -> Result<Self> {
        let (scheme, authority, path) = if let Some((scheme, rest)) = url.split_once("://") {
            ensure!(
                matches!(scheme, "https" | "http" | "ssh"),
                "PR/MR 只支持 HTTPS、HTTP 或 SSH remote"
            );
            let (authority, path) = rest.split_once('/').context("remote 缺少仓库路径")?;
            (scheme, authority, path)
        } else {
            let (authority, path) = url.split_once(':').context("PR/MR 不支持本地路径 remote")?;
            ("ssh", authority, path)
        };
        let authority = authority.rsplit('@').next().context("remote 缺少主机名")?;
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(h, p)| (h, Some(p)));
        ensure!(
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
            "remote 主机名格式不支持"
        );
        if let Some(port) = port {
            port.parse::<u16>().context("remote 端口无效")?;
        }
        let host = if scheme == "ssh" { host } else { authority }.to_ascii_lowercase();
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        ensure!(
            path.split('/').count() >= 2
                && path
                    .split('/')
                    .all(|p| !p.is_empty() && p != "." && p != "..")
                && !path
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, ':' | '?' | '#' | '%' | '\\')),
            "remote 仓库路径格式不支持"
        );
        Ok(Self {
            host,
            path: path.to_owned(),
            scheme: if scheme == "http" { "http" } else { "https" }.to_owned(),
        })
    }

    fn provider(&self, requested: Option<Provider>) -> Result<Provider> {
        let known = match self.host.as_str() {
            "github.com" => Some(Provider::Github),
            "gitlab.com" => Some(Provider::Gitlab),
            _ => None,
        };
        if let (Some(known), Some(requested)) = (known, requested) {
            ensure!(known == requested, "--provider 与 remote 的公共平台不符");
        }
        let provider = requested
            .or(known)
            .context("无法判断自建平台，请指定 --provider github 或 --provider gitlab")?;
        ensure!(
            provider != Provider::Github || self.path.split('/').count() == 2,
            "GitHub 仓库路径应为 OWNER/REPO"
        );
        Ok(provider)
    }

    fn cli_repo(&self, provider: Provider) -> String {
        match provider {
            Provider::Github => format!("{}/{}", self.host, self.path),
            Provider::Gitlab => format!("{}://{}/{}", self.scheme, self.host, self.path),
        }
    }
}

fn setting(cwd: &Path, key: &str) -> Result<Option<String>> {
    let output = git::run(
        "git",
        &[OsStr::new("config"), OsStr::new("--get"), OsStr::new(key)],
        Some(cwd),
    )?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8(output.stdout)?
                .trim_end_matches('\n')
                .to_owned(),
        )),
        Some(1) => Ok(None),
        _ => bail!(
            "无法读取 Git 配置 {key}：{}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn remote_repository(cwd: &Path, remote: &str, push: bool) -> Result<Repository> {
    let args = if push {
        vec!["remote", "get-url", "--push", "--all", "--", remote]
    } else {
        vec!["remote", "get-url", "--all", "--", remote]
    };
    let urls = git(cwd, &args)?;
    ensure!(
        urls.lines().count() == 1,
        "remote {remote} 有多个 URL，无法唯一确定 PR/MR 仓库"
    );
    Repository::parse(&urls)
}

pub fn execute(config: &Config, state: &State, invocation_dir: &Path, args: Args) -> Result<()> {
    let name = args.name;
    let feature = crate::feature(state, &name)?;
    ensure!(
        crate::check_worktree(config, feature)?,
        "任务 Worktree 不存在，请先修复"
    );
    let cwd = &feature.path;
    let remotes = git(cwd, &["remote"])?;
    let upstream = setting(cwd, &format!("branch.{}.remote", feature.branch))?;
    let remote = match args.remote.or_else(|| upstream.clone()) {
        Some(remote) => remote,
        None => {
            let names: Vec<_> = remotes.lines().collect();
            ensure!(names.len() == 1, "没有唯一 remote，请通过 --remote 指定");
            names[0].to_owned()
        }
    };
    ensure!(
        remotes.lines().any(|r| r == remote),
        "Git remote {remote} 不存在，请通过 --remote 指定"
    );
    let source = if upstream.as_deref() == Some(&remote) {
        let reference = setting(cwd, &format!("branch.{}.merge", feature.branch))?
            .context("upstream 缺少合并分支配置")?;
        reference
            .strip_prefix("refs/heads/")
            .context("upstream 不是远端分支")?
            .to_owned()
    } else {
        feature.branch.clone()
    };
    let target = args.target.unwrap_or_else(|| config.target.clone());
    for branch in [&source, &target] {
        git(cwd, &["check-ref-format", &format!("refs/heads/{branch}")])
            .context("PR/MR 分支名无效")?;
    }
    ensure!(source != target, "PR/MR 源分支与目标分支不能相同");
    let repository = remote_repository(cwd, &remote, false)?;
    let push_repository = remote_repository(cwd, &remote, true)?;
    ensure!(
        repository.host == push_repository.host && repository.path == push_repository.path,
        "fetch 与 push URL 指向不同仓库；首版不支持 Fork 跨仓库 PR/MR"
    );
    let provider = repository.provider(args.provider)?;
    let repo = repository.cli_repo(provider);
    provider.run(cwd, &["--version"])?;
    let existing = provider.existing(cwd, &repo, &source, &target)?;
    if !args.push
        && let Some(url) = existing.as_ref()
    {
        println!("{url}");
        return Ok(());
    }
    // 创建正文先准备好，再进行显式 push；路径相对于调用目录。
    let title = args
        .title
        .unwrap_or(git(cwd, &["log", "-1", "--format=%s"])?);
    ensure!(!title.trim().is_empty(), "PR/MR 标题不能为空");
    let body = match args.body_file {
        Some(path) => {
            fs::read_to_string(invocation_dir.join(path)).context("无法读取 PR/MR 正文文件")?
        }
        None => git(cwd, &["log", "-1", "--format=%B"])?,
    };
    let mut body_file = tempfile::NamedTempFile::new()?;
    body_file.write_all(body.as_bytes())?;
    let source_ref = format!("refs/heads/{source}");
    let target_ref = format!("refs/heads/{target}");
    let refs = git(
        cwd,
        &[
            "ls-remote",
            "--heads",
            "--",
            &remote,
            &source_ref,
            &target_ref,
        ],
    )?;
    let remote_oid = |reference: &str| {
        refs.lines()
            .filter_map(|line| line.split_once('\t'))
            .find(|(_, r)| *r == reference)
            .map(|(oid, _)| oid)
    };
    ensure!(
        remote_oid(&target_ref).is_some(),
        "remote {remote} 缺少目标分支 {target}"
    );
    if args.push {
        eprintln!("推送 {} → {remote}/{source}", feature.branch);
        let status = Command::new("git")
            .current_dir(cwd)
            .args([
                "push",
                "--no-follow-tags",
                "--set-upstream",
                "--",
                &remote,
                &format!("refs/heads/{}:{source_ref}", feature.branch),
            ])
            .status()
            .context("无法执行 git push")?;
        ensure!(status.success(), "推送失败，未创建 PR/MR；未强制覆盖远端");
    } else {
        let head = git(cwd, &["rev-parse", "HEAD"])?;
        ensure!(
            remote_oid(&source_ref) == Some(head.as_str()),
            "远端源分支缺失或与本地提交不同，请先推送，或显式添加 --push"
        );
    }
    if let Some(url) = existing {
        println!("{url}");
        return Ok(());
    }
    let file = body_file.path().to_str().context("暂不支持非 UTF-8 路径")?;
    let result = match provider {
        Provider::Github => provider.run(
            cwd,
            &[
                "pr",
                "create",
                "--repo",
                &repo,
                "--head",
                &source,
                "--base",
                &target,
                "--draft",
                "--title",
                &title,
                "--body-file",
                file,
            ],
        ),
        Provider::Gitlab => provider.run(
            cwd,
            &[
                "mr",
                "create",
                "--repo",
                &repo,
                "--head",
                &repo,
                "--source-branch",
                &source,
                "--target-branch",
                &target,
                "--draft",
                "--push=false",
                "--yes",
                "--title",
                &title,
                "--description-file",
                file,
            ],
        ),
    }
    .context("创建 PR/MR 失败；已推送的提交会保留，可修复后重试")?;
    println!("{result}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_keep_the_repository_identity_without_credentials() {
        for (url, host, path) in [
            (
                "git@github.com:team/project.git",
                "github.com",
                "team/project",
            ),
            (
                "https://user:secret@github.com/team/project.git",
                "github.com",
                "team/project",
            ),
            (
                "ssh://git@git.example.test:2222/group/sub/project.git",
                "git.example.test",
                "group/sub/project",
            ),
            (
                "https://git.example.test:8443/group/sub/project.git",
                "git.example.test:8443",
                "group/sub/project",
            ),
        ] {
            let repo = Repository::parse(url).unwrap();
            assert_eq!(repo.host, host);
            assert_eq!(repo.path, path);
        }
        for url in [
            "/tmp/repo",
            "file:///tmp/repo",
            "https://github.com/team",
            "https://github.com/team/repo?token=secret",
            "https://github.com/team/../repo",
        ] {
            assert!(Repository::parse(url).is_err(), "应拒绝 {url}");
        }
    }
}
