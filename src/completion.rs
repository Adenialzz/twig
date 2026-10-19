use crate::{Cli, config::Project, git::git, registry};
use anyhow::{Context, Result};
use clap::{ArgMatches, CommandFactory};
use clap_complete::{
    CompleteEnv, CompletionCandidate,
    env::{self, EnvCompleter, Shells},
};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

pub fn complete() {
    CompleteEnv::with_factory(Cli::command)
        .shells(Shells(&[
            &Zsh,
            &env::Bash,
            &env::Elvish,
            &env::Fish,
            &env::Powershell,
        ]))
        .complete();
}

// 候选协议仍由 Clap 处理；只定制 Zsh 的分组展示。
struct Zsh;

impl EnvCompleter for Zsh {
    fn name(&self) -> &'static str {
        "zsh"
    }

    fn is(&self, name: &str) -> bool {
        name == "zsh"
    }

    fn write_registration(
        &self,
        var: &str,
        _name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn io::Write,
    ) -> io::Result<()> {
        // 单引号保护可执行文件路径中的空格、引号及 Shell 元字符。
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        writeln!(buf, "_twig_complete() {{")?;
        // Q 只去除词中的 Shell 引号和转义，不执行变量或命令替换。
        writeln!(
            buf,
            "    local -a completions=(\"${{(@f)$( _CLAP_COMPLETE_INDEX=$((CURRENT - 1)) _CLAP_IFS=$'\\n' {var}=zsh {} -- \"${{(@Q)words}}\" 2>/dev/null )}}\")",
            quote(completer)
        )?;
        buf.write_all(include_bytes!("completion.zsh"))?;
        writeln!(buf, "}}\ncompdef _twig_complete {}", quote(bin))
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&Path>,
        buf: &mut dyn io::Write,
    ) -> io::Result<()> {
        env::Zsh.write_complete(cmd, args, current_dir, buf)
    }
}

// CompleteEnv 在 -- 后传入原命令行；只解析光标前已完成的词。
// ignore_errors 允许子命令尚缺必填参数，参数语义仍由同一份 CLI 定义负责。
fn context() -> Result<ArgMatches> {
    let words: Vec<_> = std::env::args_os()
        .skip_while(|word| word != "--")
        .skip(1)
        .collect();
    let index = std::env::var("_CLAP_COMPLETE_INDEX")?.parse::<usize>()?;
    Ok(Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(words.into_iter().take(index))?)
}

fn registry_path(matches: &ArgMatches) -> Result<PathBuf> {
    registry::location(matches.get_one::<PathBuf>("config").cloned())
}

fn repo_path(matches: &ArgMatches) -> Result<PathBuf> {
    let (action, command) = matches.subcommand().context("缺少命令")?;
    if action == "add" {
        return registry::expand(
            command
                .get_one::<PathBuf>("path")
                .context("请先输入 --path")?,
        );
    }
    let name = command.get_one::<String>("project").context("缺少项目名")?;
    registry::expand(&registry::entry(&registry_path(matches)?, name)?.path)
}

fn candidates(
    current: &OsStr,
    query: impl FnOnce(&ArgMatches) -> Result<Vec<String>>,
) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return Vec::new();
    };
    // Clap 可能同时尝试位置参数；补全选项时不必查询项目状态。
    if prefix.starts_with('-') {
        return Vec::new();
    }
    match context().and_then(|matches| query(&matches)) {
        Ok(values) => values
            .into_iter()
            .filter(|value| value.starts_with(prefix))
            .map(CompletionCandidate::new)
            .collect(),
        Err(error) => {
            // Shell 隐藏补全诊断；直接调用补全协议时仍可从 stderr 排查。
            eprintln!("补全失败：{error:#}");
            Vec::new()
        }
    }
}

pub fn projects(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(current, |matches| {
        Ok(registry::load(&registry_path(matches)?)?
            .projects
            .into_keys()
            .collect())
    })
}

pub fn tasks(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(current, |matches| {
        Ok(
            Project::discover_at(&repo_path(matches)?, &registry_path(matches)?)?
                .state()?
                .into_keys()
                .collect(),
        )
    })
}

pub fn branches(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(current, |matches| {
        Ok(git(
            &repo_path(matches)?,
            &["for-each-ref", "--format=%(refname:strip=2)", "refs/heads/"],
        )?
        .lines()
        .map(str::to_owned)
        .collect())
    })
}

pub fn git_remotes(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(current, |matches| {
        Ok(git(&repo_path(matches)?, &["remote"])?
            .lines()
            .map(str::to_owned)
            .collect())
    })
}
