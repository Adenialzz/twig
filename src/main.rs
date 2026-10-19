mod codex;
mod completion;
mod config;
mod console;
mod git;
mod pr;
mod registry;
mod tmux;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use clap_complete::ArgValueCompleter;
use config::{Config, Feature, Project, State};
use git::git;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "twig", version, about = "Git Worktree + tmux 任务环境管理")]
struct Cli {
    /// 在任意目录操作已登记项目
    #[arg(long, global = true, add = ArgValueCompleter::new(completion::projects))]
    repo: Option<String>,
    /// 使用其他登记表（默认 ~/.twig/config.toml）
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// 全局项目、任务与 Codex 会话控制台
    Console,
    /// 控制台登记表单的独立进程入口
    #[command(name = "__console-register", hide = true)]
    ConsoleRegister {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        name: String,
    },
    /// 管理用户级项目登记表
    Config {
        #[command(subcommand)]
        command: registry::Action,
    },
    /// 克隆登记的项目、添加 remotes 并初始化 twig
    Clone {
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        name: String,
    },
    /// 初始化项目（不修改 Git 分支，不拉取远端）
    Init {
        /// 全局登记别名，默认使用主仓库目录名
        #[arg(long)]
        name: Option<String>,
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        base: Option<String>,
        /// 默认合并目标，默认与基础分支相同
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        target: Option<String>,
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        root: Option<PathBuf>,
    },
    /// 创建并进入任务 Shell 窗口；重复执行接续已有环境
    New {
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        base: Option<String>,
        /// 只创建或接续任务环境，不进入窗口
        #[arg(long)]
        no_open: bool,
    },
    /// 列出由 twig 管理的任务及实际状态
    Ls,
    /// 查找或创建草稿 PR/MR；仅 --push 才推送提交
    Pr(pr::Args),
    /// 进入任务窗口；窗口丢失时重建 Shell
    Open {
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
    },
    /// 关闭窗口及其中的进程，保留 Worktree 和分支
    Stop {
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 安全移除 Worktree 和已合并分支
    Rm {
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
        #[command(flatten)]
        options: RemoveOptions,
        /// 控制台确认时的仓库、分支和路径快照
        #[arg(long, hide = true)]
        expected_task: Option<String>,
    },
}

#[derive(clap::Args)]
struct RemoveOptions {
    #[arg(long)]
    keep_branch: bool,
    /// 同时删除 Worktree 中被 Git 忽略的文件（仍保护未提交和未跟踪文件）
    #[arg(long)]
    delete_ignored: bool,
    #[arg(long, hide = true)]
    expected_ignored: Option<String>,
    #[arg(long, add = ArgValueCompleter::new(completion::branches))]
    target: Option<String>,
    #[arg(short = 'y', long)]
    yes: bool,
}

fn main() {
    completion::complete();
    if let Err(error) = execute(Cli::parse()) {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

fn execute(cli: Cli) -> Result<()> {
    let invocation_dir = std::env::current_dir()?;
    let action = match cli.command.unwrap_or(Action::Console) {
        Action::Console => {
            return console::run(registry::location(cli.config)?, cli.repo, invocation_dir);
        }
        Action::ConsoleRegister { path, name } => {
            ensure!(cli.repo.is_none(), "登记表单不接受 --repo");
            return console::form::register(&registry::location(cli.config)?, &path, name);
        }
        Action::Config { command } => {
            ensure!(
                cli.repo.is_none(),
                "config 命令直接使用项目名，不需要 --repo"
            );
            return registry::execute(&registry::location(cli.config)?, command);
        }
        Action::Clone { name } => {
            ensure!(
                cli.repo.is_none(),
                "clone 命令直接使用项目名，不需要 --repo"
            );
            let path = registry::location(cli.config)?;
            let entry = registry::entry(&path, &name)?;
            return registry::clone(&entry);
        }
        action => action,
    };
    let registry_path = if cli.repo.is_some() || matches!(action, Action::Init { .. }) {
        Some(registry::location(cli.config)?)
    } else {
        None
    };
    let registered = if let Some(name) = &cli.repo {
        let entry = registry::entry(registry_path.as_ref().unwrap(), name)?;
        let path = registry::expand(&entry.path)?;
        std::env::set_current_dir(&path).with_context(|| {
            format!(
                "无法进入 {}；尚未克隆时请运行 twig clone {name}",
                path.display()
            )
        })?;
        let root = git(&path, &["rev-parse", "--show-toplevel"])?;
        ensure!(
            PathBuf::from(root).canonicalize()? == path.canonicalize()?,
            "登记路径不是仓库根目录"
        );
        Some(entry)
    } else {
        None
    };
    let project = Project::discover()?;
    if matches!(action, Action::Init { .. }) && project.dir.join("config.json").try_exists()? {
        println!("当前项目已经初始化，无需重复执行。");
        return Ok(());
    }
    let lock = project.lock()?;
    if let Action::Init {
        name,
        base,
        target,
        root,
    } = action
    {
        let base = base.or_else(|| registered.as_ref().map(|r| r.main.clone()));
        let target = target.or_else(|| registered.as_ref().map(|r| r.main.clone()));
        return registry::initialize(
            registry_path.as_ref().unwrap(),
            name.or(cli.repo),
            &project,
            || init(&project, base, target, root),
        );
    }
    let config = project.config()?;
    let mut state = project.state()?;
    match action {
        Action::New {
            name,
            base,
            no_open,
        } => {
            let window = new(&project, &config, &mut state, &name, base)?;
            drop(lock); // attach 不能持有项目锁。
            if no_open {
                Ok(())
            } else {
                tmux::enter(&config, &window)
            }
        }
        Action::Ls => list(&project, &config, &state),
        Action::Pr(args) => pr::execute(&config, &state, &invocation_dir, args),
        Action::Open { name } => {
            let feature = feature(&state, &name)?;
            ensure!(
                check_worktree(&config, feature)?,
                "Worktree 不存在；请检查 twig ls，必要时重试 twig new {name}"
            );
            let window = tmux::ensure_window(&config, &project, &name, &feature.path)?;
            drop(lock); // attach 不能持有项目锁。
            tmux::enter(&config, &window)
        }
        Action::Stop { name, yes } => {
            feature(&state, &name)?;
            if let Some(window) = tmux::window(&config, &project, &name)? {
                confirm(&format!("关闭 {name} 窗口并终止其中的全部进程？"), yes)?;
                tmux::tmux(&["kill-window", "-t", &window])?;
            }
            println!("已停止 {name}；Worktree 和分支保留");
            Ok(())
        }
        Action::Rm {
            name,
            options,
            expected_task,
        } => {
            if let Some(expected) = expected_task {
                let (common, branch, path): (PathBuf, String, PathBuf) =
                    serde_json::from_str(&expected)?;
                let task = feature(&state, &name)?;
                ensure!(
                    project.common == common && task.branch == branch && task.path == path,
                    "任务归属已变化，请取消并重新确认删除"
                );
            }
            remove(&project, &config, &mut state, &name, &options)
        }
        Action::Console
        | Action::ConsoleRegister { .. }
        | Action::Init { .. }
        | Action::Config { .. }
        | Action::Clone { .. } => {
            unreachable!()
        }
    }
}

fn init(
    project: &Project,
    base: Option<String>,
    target: Option<String>,
    root: Option<PathBuf>,
) -> Result<()> {
    if project.dir.join("config.json").exists() {
        let config = project.config()?;
        ensure!(
            base.as_ref().is_none_or(|b| b == &config.base)
                && target.as_ref().is_none_or(|t| t == &config.target)
                && root.is_none(),
            "项目已初始化；如需修改配置，请编辑 {}",
            project.dir.join("config.json").display()
        );
        tmux::ensure_session(&config, project)?;
        println!("项目已初始化：{}", project.dir.display());
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    ensure!(
        git(&cwd, &["rev-parse", "--is-bare-repository"])? == "false",
        "暂不支持裸仓库"
    );
    let main = git::worktrees(&cwd)?
        .first()
        .context("未找到主工作目录")?
        .0
        .canonicalize()?;
    let base = match base {
        Some(value) => value,
        None if git::exists(&main, "main")? => "main".to_owned(),
        None if git::exists(&main, "master")? => "master".to_owned(),
        None => bail!("未找到 main/master，请通过 --base 指定已有本地分支"),
    };
    ensure!(git::exists(&main, &base)?, "基础分支不存在：{base}");
    let target = target.unwrap_or_else(|| base.clone());
    ensure!(git::exists(&main, &target)?, "合并目标分支不存在：{target}");
    let root = root.unwrap_or_else(|| {
        main.with_file_name(format!(
            "{}.worktrees",
            main.file_name().unwrap().to_string_lossy()
        ))
    });
    let root = if root.is_absolute() {
        root
    } else {
        cwd.join(root)
    };
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    ensure!(
        !root.starts_with(&main) && !main.starts_with(&root),
        "Worktree 根目录必须与主工作目录分离，不能相互包含"
    );
    ensure!(
        root.to_str().is_some() && main.to_str().is_some() && project.common.to_str().is_some(),
        "暂不支持非 UTF-8 路径"
    );
    let config = Config {
        version: 1,
        main,
        root,
        base,
        target,
        session: config::session(&project.common),
    };
    project.save("config.json", &config)?;
    tmux::ensure_session(&config, project).context(
        "配置已保存；修复 tmux 后可通过 new/open 创建窗口；全局登记缺失时请手动补充登记表",
    )?;
    println!(
        "已初始化：{}\nWorktree 根目录：{}\ntmux Session：{}",
        project.dir.display(),
        config.root.display(),
        config.session
    );
    Ok(())
}

fn feature<'a>(state: &'a State, name: &str) -> Result<&'a Feature> {
    config::task_name(name)?;
    state
        .get(name)
        .with_context(|| format!("没有任务 {name}，请运行 twig ls"))
}

fn check_worktree(config: &Config, feature: &Feature) -> Result<bool> {
    let trees = git::worktrees(&config.main)?;
    if let Some((_, branch)) = trees.iter().find(|(path, _)| path == &feature.path) {
        ensure!(
            feature.path.is_dir(),
            "Worktree 记录存在但目录丢失：{}；请先检查 git worktree prune --dry-run，再手动修复或清理失效记录",
            feature.path.display()
        );
        ensure!(
            branch == &feature.branch,
            "Worktree 分支已改变，拒绝操作：{}",
            feature.path.display()
        );
        let common = PathBuf::from(git(
            &feature.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()?;
        let expected = PathBuf::from(git(
            &config.main,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)
        .canonicalize()?;
        ensure!(
            common == expected && feature.path.canonicalize()? == feature.path,
            "Worktree 路径归属异常，拒绝操作"
        );
        return Ok(true);
    }
    ensure!(
        !feature.path.exists(),
        "路径已存在但不属于登记的 Worktree：{}",
        feature.path.display()
    );
    Ok(false)
}

// 斜杠任务使用子目录；中间目录不得是符号链接或另一个仓库。
fn check_task_parent(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .context("任务路径不在 Worktree 根目录内")?;
    let mut parent = root.to_path_buf();
    for part in relative.parent().into_iter().flat_map(Path::components) {
        parent.push(part);
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "任务父路径不是普通目录：{}",
                    parent.display()
                );
                match std::fs::symlink_metadata(parent.join(".git")) {
                    Ok(_) => bail!("任务父目录属于已有仓库：{}", parent.display()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn new(
    project: &Project,
    config: &Config,
    state: &mut State,
    name: &str,
    base: Option<String>,
) -> Result<String> {
    config::task_name(name)?;
    ensure!(name != "main", "main 保留给项目主窗口");
    if let Some(existing) = state.get(name) {
        ensure!(
            base.as_ref().is_none_or(|b| b == &existing.base),
            "任务已存在，不能更改基础分支"
        );
    } else {
        let base = base.unwrap_or_else(|| config.base.clone());
        ensure!(git::exists(&config.main, &base)?, "基础分支不存在：{base}");
        let branch = name.to_owned();
        git(&config.main, &["check-ref-format", "--branch", &branch])?;
        ensure!(
            !git::exists(&config.main, &branch)?,
            "分支已存在：{branch}；不会接管外部已有分支"
        );
        let path = config.root.join(name);
        check_task_parent(&config.root, &path)?;
        ensure!(
            state
                .values()
                .all(|task| !path.starts_with(&task.path) && !task.path.starts_with(&path)),
            "任务路径与已有任务相互包含"
        );
        ensure!(
            !path.try_exists()? && std::fs::symlink_metadata(&path).is_err(),
            "路径已存在：{}",
            path.display()
        );
        // 先保存意图，确保外部命令部分成功后仍能通过同名 new 接续。
        state.insert(name.to_owned(), Feature { branch, base, path });
        project.save("state.json", state)?;
    }
    let feature = feature(state, name)?;
    if !check_worktree(config, feature)? {
        check_task_parent(&config.root, &feature.path)?;
        let path = feature.path.to_str().context("暂不支持非 UTF-8 路径")?;
        if git::exists(&config.main, &feature.branch)? {
            git(
                &config.main,
                &["worktree", "add", "--", path, &feature.branch],
            )?;
        } else {
            git(
                &config.main,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &feature.branch,
                    "--",
                    path,
                    &format!("refs/heads/{}", feature.base),
                ],
            )?;
        }
    }
    let window = tmux::ensure_window(config, project, name, &feature.path).with_context(|| {
        format!(
            "Worktree 已保留在 {}；修复后执行 twig new {name} 或 twig open {name}",
            feature.path.display()
        )
    })?;
    println!(
        "{name}\n分支：{}\n目录：{}\n进入：twig open {name}",
        feature.branch,
        feature.path.display()
    );
    Ok(window)
}

fn list(project: &Project, config: &Config, state: &State) -> Result<()> {
    println!(
        "{:<20} {:<28} {:<12} {:<12} 路径",
        "任务", "分支", "tmux", "Git"
    );
    for (name, feature) in state {
        let status = match check_worktree(config, feature) {
            Ok(true) => {
                if git(
                    &feature.path,
                    &["status", "--porcelain", "--untracked-files=all"],
                )?
                .is_empty()
                {
                    "Clean"
                } else {
                    "Dirty"
                }
            }
            Ok(false) => "Missing",
            Err(error) => {
                eprintln!("{name}: {error:#}");
                "Mismatch"
            }
        };
        let window = tmux::window(config, project, name)?;
        println!(
            "{:<20} {:<28} {:<12} {:<12} {}",
            name,
            feature.branch,
            if window.is_some() { "Open" } else { "Closed" },
            status,
            feature.path.display()
        );
    }
    Ok(())
}

fn confirm(message: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    print!("{message} [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(matches!(answer.trim(), "y" | "Y"), "操作已取消");
    Ok(())
}

fn remove(
    project: &Project,
    config: &Config,
    state: &mut State,
    name: &str,
    options: &RemoveOptions,
) -> Result<()> {
    let keep_branch = options.keep_branch;
    let yes = options.yes;
    let target = options.target.clone();
    config::task_name(name)?;
    let Some(feature) = state.get(name).cloned() else {
        println!("任务 {name} 已不存在；未操作任何外部资源");
        return Ok(());
    };
    let target = target.unwrap_or_else(|| config.target.clone());
    let validate = |report: bool| -> Result<(bool, bool)> {
        let present = check_worktree(config, &feature)?;
        if present {
            let dirty = git(
                &feature.path,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                    if options.delete_ignored {
                        "--ignored=no"
                    } else {
                        "--ignored=matching"
                    },
                ],
            )?;
            ensure!(
                dirty.is_empty(),
                "Worktree 有未提交、未跟踪或被忽略的文件；请先保存或手动清理：\n{dirty}"
            );
        }
        if present && options.delete_ignored {
            let ignored = git::ignored(&feature.path)?;
            if let Some(expected) = &options.expected_ignored {
                let expected: Vec<String> = serde_json::from_str(expected)?;
                ensure!(
                    ignored == expected,
                    "被忽略文件清单已变化，请取消并重新打开删除确认"
                );
            }
            if report && !ignored.is_empty() {
                eprintln!(
                    "同时删除以下被忽略文件/目录（包含目录内全部内容）：\n{}",
                    ignored.join("\n")
                );
            }
        }
        let branch_exists = git::exists(&config.main, &feature.branch)?;
        if branch_exists && !keep_branch {
            ensure!(
                git::exists(&config.main, &target)?,
                "合并目标不存在：{target}"
            );
            ensure!(
                git::merged(
                    &config.main,
                    &format!("refs/heads/{}", feature.branch),
                    &format!("refs/heads/{target}")
                )?,
                "{} 尚未合并到 {target}；可用 --keep-branch 保留分支",
                feature.branch
            );
            let upstream = git(
                &config.main,
                &[
                    "for-each-ref",
                    "--format=%(upstream)",
                    &format!("refs/heads/{}", feature.branch),
                ],
            )?;
            if upstream.is_empty() && report {
                eprintln!("提示：未配置 upstream，无法验证推送状态；已验证合并到本地 {target}。");
            } else if !upstream.is_empty() {
                let ahead = git(
                    &config.main,
                    &[
                        "rev-list",
                        "--count",
                        &format!("{upstream}..refs/heads/{}", feature.branch),
                    ],
                )?;
                ensure!(
                    ahead == "0",
                    "相对本地 upstream 记录还有 {ahead} 个未推送提交；请先推送或使用 --keep-branch"
                );
            }
        }
        Ok((present, branch_exists))
    };
    validate(true)?;
    let window = tmux::window(config, project, name)?;
    if let Some(id) = &window {
        ensure!(
            tmux::idle(id)?,
            "窗口仍有活动进程或状态未知；请先退出任务，或明确执行 twig stop {name}"
        );
    }
    confirm(
        &format!(
            "移除 {name} 的窗口和 Worktree{}{}？",
            if options.delete_ignored {
                "（包含被忽略文件）"
            } else {
                ""
            },
            if keep_branch {
                "，保留分支"
            } else {
                "及已合并分支"
            }
        ),
        yes,
    )?;
    // 确认期间可能发生外部写入；再次验证，包括 Git remove 默认不保护的 ignored 文件。
    let (present, branch_exists) = validate(false)?;
    ensure!(
        tmux::window(config, project, name)? == window,
        "窗口状态已变化，取消清理"
    );
    if let Some(id) = &window {
        ensure!(tmux::idle(id)?, "窗口状态已变化，取消清理");
        tmux::tmux(&["kill-window", "-t", id])?;
    }
    if present {
        git(
            &config.main,
            &["worktree", "remove", "--", feature.path.to_str().unwrap()],
        )
        .context("窗口可能已关闭；Worktree 和记录已保留，可修复后重试")?;
    }
    if branch_exists && !keep_branch {
        git(&config.main, &["branch", "-d", "--", &feature.branch])
            .context("Worktree 已移除，但 Git 拒绝安全删除分支；分支和 twig 记录保留，可用 rm --keep-branch 完成清理")?;
    }
    state.remove(name);
    project.save("state.json", state)?;
    println!(
        "已移除 {name}{}",
        if keep_branch {
            "（分支保留）"
        } else {
            ""
        }
    );
    Ok(())
}
