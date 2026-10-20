mod codex;
mod completion;
mod config;
mod console;
mod git;
mod guide;
mod pr;
mod registry;
mod startup;
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
#[command(
    name = "twig",
    version,
    about = "Git Worktree + tmux 任务环境管理",
    after_help = "项目通过登记别名定位；任务命令显式指定项目和任务。裸 twig 打开交互控制台。\n\n开始使用：\n  twig add myapp --path /path/to/repo\n  twig new myapp auth\n  twig ls myapp\n\n深入阅读：twig <命令> --help；twig guide；twig guide --all"
)]
struct Cli {
    /// 使用其他登记表及同目录项目状态（默认 ~/.twig/config.toml）
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// 阅读离线用户手册；不带主题时显示导航
    #[command(
        after_help = "示例：twig guide quickstart\n      twig guide windows\n      twig guide --all | less\n\n输出 Markdown 文本，不读取配置、不联网、不调用 Git/tmux，也不自动打开分页器。"
    )]
    Guide {
        /// 要阅读的主题；省略时显示手册目录
        #[arg(value_enum)]
        topic: Option<guide::Topic>,
        /// 输出目录及所有主题，不能与主题同时使用
        #[arg(long, conflicts_with = "topic")]
        all: bool,
    },
    /// 全局项目、任务与 Codex 会话控制台
    #[command(
        after_help = "需要交互终端。浏览只读；提交表单、确认删除和进入缺失窗口会操作资源。\n完整操作说明：twig guide console；脚本请使用 twig ls。"
    )]
    Console,
    /// 登记并初始化已有 Git 仓库（不联网）
    #[command(
        after_help = "需要已有提交的非裸仓库。同路径复用原别名，别名冲突不覆盖。\n首次初始化创建配置、根目录及主窗口；已有配置时只补登记，冲突初始化参数拒绝。\n示例：twig add myapp --path ~/projects/myapp --base develop\n详细规则：twig guide commands；twig guide configuration"
    )]
    Add {
        /// 项目别名：1–64 位 ASCII 字母数字、横线或下划线，首位为字母数字
        name: String,
        /// 已有仓库路径；支持 ~/ 和相对调用目录的路径
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        path: PathBuf,
        /// 首次初始化的已有本地基础分支；默认 main，其次 master
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        base: Option<String>,
        /// 安全删除的本地合并检查目标；默认等于基础分支
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        target: Option<String>,
        /// Worktree 根目录；默认主仓库旁的 <目录名>.worktrees，不能与主目录相互包含
        #[arg(long, value_hint = clap::ValueHint::DirPath)]
        root: Option<PathBuf>,
        #[arg(long, hide = true)]
        print_name: bool,
    },
    /// 创建并进入任务 Shell 窗口；重复执行接续已有环境
    #[command(
        after_help = "任务名直接作为分支名，可用 / 分段，总长 1–64；每段为 ASCII 字母数字、横线或下划线且首位为字母数字，main 保留。\n只接续自己登记的资源，不接管外部同名分支；失败保留已创建资源。\n示例：twig new myapp feat/login --no-open\n启动与重试：twig guide windows；twig guide troubleshooting"
    )]
    New {
        /// 已登记的项目别名，不根据当前目录推断
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        project: String,
        /// 任务名
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
        /// 已有本地起点分支；默认项目 base，不联网更新分支
        #[arg(long, add = ArgValueCompleter::new(completion::branches))]
        base: Option<String>,
        /// 不进入窗口；仍创建 Pane 并执行配置的启动命令（不是 dry-run）
        #[arg(long)]
        no_open: bool,
    },
    /// 列出项目；指定项目时列出其任务及实际状态
    #[command(
        after_help = "Open 只表示窗口存在；Clean 不包含 ignored 文件，不代表已合并/推送。\nMissing/Mismatch 请检查目录和 stderr 诊断；Mismatch 可能伴随成功退出码。\n示例：twig ls；twig ls myapp\n详细状态：twig guide commands"
    )]
    Ls {
        /// 已登记的项目别名，不根据当前目录推断
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        project: Option<String>,
    },
    /// 查找或创建草稿 PR/MR；仅 --push 才推送提交
    Pr(pr::Args),
    /// 进入任务窗口；窗口丢失时重建 Shell
    #[command(
        after_help = "窗口缺失时会执行当前启动配置；Worktree 缺失或归属错误时报错。\n不恢复原进程或对话。通常需要交互终端；脚本准备环境用 new --no-open。\n示例：twig open myapp auth\n详细说明：twig guide windows"
    )]
    Open {
        /// 已登记的项目别名，不根据当前目录推断
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        project: String,
        /// 任务名
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
    },
    /// 关闭窗口及其中的进程，保留 Worktree 和分支
    #[command(
        after_help = "默认确认后终止窗口内全部进程，保留代码、分支和任务记录。\n重新准备窗口用 open 或 new --no-open；缺失窗口不重建。\n示例：twig stop myapp auth\n详细说明：twig guide cleanup"
    )]
    Stop {
        /// 已登记的项目别名，不根据当前目录推断
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        project: String,
        /// 任务名
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: String,
        /// 跳过终止进程的确认
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// 取消项目登记；指定任务时安全删除该任务
    #[command(
        after_help = "rm PROJECT 立即取消登记，保留所有资源；rm PROJECT NAME 确认后安全删除任务。\n默认保护未提交、未跟踪、ignored 文件和活动进程，检查合并及本地 upstream；不提供强制删除。\n--target/--keep-branch/--delete-ignored 必须指定任务名。部分失败保留剩余资源。\n示例：twig rm myapp auth --keep-branch\n完整清理条件：twig guide cleanup"
    )]
    Rm {
        /// 已登记的项目别名，不根据当前目录推断
        #[arg(add = ArgValueCompleter::new(completion::projects))]
        project: String,
        /// 任务名；rm 中省略时只取消项目登记
        #[arg(add = ArgValueCompleter::new(completion::tasks))]
        name: Option<String>,
        #[command(flatten)]
        options: RemoveOptions,
        /// 控制台确认时的仓库、分支和路径快照
        #[arg(long, hide = true)]
        expected_task: Option<String>,
    },
}

#[derive(clap::Args)]
struct RemoveOptions {
    /// 保留本地分支，跳过合并/upstream 检查，仍保护文件和进程
    #[arg(long)]
    keep_branch: bool,
    /// 同时删除 Worktree 中被 Git 忽略的文件（仍保护未提交和未跟踪文件）
    #[arg(long)]
    delete_ignored: bool,
    #[arg(long, hide = true)]
    expected_ignored: Option<String>,
    /// 本次合并检查的已有本地目标分支；默认项目 target
    #[arg(long, add = ArgValueCompleter::new(completion::branches))]
    target: Option<String>,
    /// 仅跳过确认，不绕过删除保护
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
    if let Some(Action::Guide { topic, all }) = cli.command {
        return Ok(guide::print(topic, all)?);
    }
    let invocation_dir = std::env::current_dir()?;
    let registry_path = registry::location(cli.config)?;
    let action = cli.command.unwrap_or(Action::Console);
    match action {
        Action::Console => return console::run(registry_path, invocation_dir),
        Action::Add {
            name,
            path,
            base,
            target,
            root,
            print_name,
        } => {
            let name = registry::add(&registry_path, &path, name, base, target, root)?;
            if print_name {
                println!("{}", serde_json::to_string(&name)?);
            }
            return Ok(());
        }
        Action::Ls { project: None } => return registry::list(&registry_path),
        Action::Rm {
            ref project,
            name: None,
            ref options,
            ref expected_task,
        } => {
            ensure!(
                !options.keep_branch
                    && !options.delete_ignored
                    && options.target.is_none()
                    && options.expected_ignored.is_none()
                    && expected_task.is_none(),
                "任务删除选项必须指定任务名；取消登记请使用 twig rm <项目名>"
            );
            return registry::remove(&registry_path, project);
        }
        _ => {}
    }
    let project_name = match &action {
        Action::New { project, .. }
        | Action::Open { project, .. }
        | Action::Stop { project, .. }
        | Action::Rm { project, .. } => project,
        Action::Ls {
            project: Some(project),
        } => project,
        Action::Pr(args) => &args.project,
        _ => unreachable!(),
    };
    let entry = registry::entry(&registry_path, project_name)?;
    let path = registry::expand(&entry.path)?;
    let root = git(&path, &["rev-parse", "--show-toplevel"])?;
    ensure!(
        PathBuf::from(root).canonicalize()? == path.canonicalize()?,
        "登记路径不是仓库根目录"
    );
    let project = Project::discover_at(&path, &registry_path)?;
    let lock = project.lock()?;
    let config = project.config()?;
    let mut state = project.state()?;
    match action {
        Action::New {
            name,
            base,
            no_open,
            project: alias,
        } => {
            let window = new(&project, &config, &mut state, &alias, &name, base)?;
            drop(lock); // attach 不能持有项目锁。
            if no_open {
                Ok(())
            } else {
                tmux::enter(&config, &window)
            }
        }
        Action::Ls { .. } => list(&project, &config, &state),
        Action::Pr(args) => pr::execute(&config, &state, &invocation_dir, args),
        Action::Open { name, .. } => {
            let feature = feature(&state, &name)?;
            ensure!(
                check_worktree(&config, feature)?,
                "Worktree 不存在；请检查 twig ls <项目名>，必要时重试 twig new <项目名> {name}"
            );
            let window = tmux::ensure_window(&config, &project, &name, &feature.path)?;
            drop(lock); // attach 不能持有项目锁。
            tmux::enter(&config, &window)
        }
        Action::Stop { name, yes, .. } => {
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
            ..
        } => {
            let name = name.unwrap();
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
        Action::Guide { .. } | Action::Console | Action::Add { .. } => unreachable!(),
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
    tmux::ensure_session(&config, project)
        .context("配置已保存；修复 tmux 后可通过 new/open 创建窗口；登记未完成时可重试 twig add")?;
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
        .with_context(|| format!("没有任务 {name}，请运行 twig ls <项目名>"))
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
    alias: &str,
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
            "Worktree 已保留在 {}；修复后执行 twig new {alias} {name} 或 twig open {alias} {name}",
            feature.path.display()
        )
    })?;
    println!(
        "{name}\n分支：{}\n目录：{}\n进入：twig open {alias} {name}",
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
