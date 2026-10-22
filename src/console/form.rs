//! 表单在独立 twig 进程中执行写操作，复用 CLI 的锁和生命周期逻辑。
use crate::{git::git, registry};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
    process::{Child, Command, Stdio},
};

pub struct Form {
    pub project: Option<String>,
    pub fields: [String; 2],
    pub field: usize,
    pub error: Option<String>,
    pub scroll: u16,
    pub directories: Vec<String>,
    pub directory_selection: usize,
    pub directory_error: Option<String>,
    pub branches: Vec<BranchCandidate>,
    pub branch_selection: usize,
    pub branch_error: Option<String>,
    branch_input: String,
    selected_branch: Option<BranchCandidate>,
    pub operation_started: bool,
    pub deletion: Option<Deletion>,
    pub existing: Option<ExistingBranch>,
    default_alias: Option<String>,
    completion_input: Option<String>,
    job: Option<Job>,
}
#[derive(Clone)]
pub struct BranchCandidate {
    pub name: String,
    pub label: String,
}

impl BranchCandidate {
    fn reference(&self) -> String {
        if self.label == self.name {
            format!("refs/heads/{}", self.name)
        } else {
            format!("refs/remotes/{}", self.label)
        }
    }
}

pub struct ExistingBranch {
    pub branches: Vec<crate::config::BranchStart>,
    pub common: std::path::PathBuf,
    pub selected: usize,
    pub confirmed: bool,
}
pub struct Deletion {
    pub branch: String,
    pub path: std::path::PathBuf,
    pub expected: String,
    pub delete_branch: bool,
    pub delete_ignored: bool,
    pub ignored: Vec<String>,
    pub confirm: bool,
    pub option: usize,
}
struct Job {
    child: Child,
    output: File,
}
impl Form {
    pub fn project() -> Self {
        Self {
            project: None,
            fields: Default::default(),
            field: 0,
            error: None,
            scroll: 0,
            directories: Vec::new(),
            directory_selection: 0,
            directory_error: None,
            branches: Vec::new(),
            branch_selection: 0,
            branch_error: None,
            branch_input: String::new(),
            selected_branch: None,
            operation_started: false,
            deletion: None,
            existing: None,
            default_alias: None,
            completion_input: None,
            job: None,
        }
    }
    pub fn task(project: String, base: String) -> Self {
        Self {
            project: Some(project),
            fields: [String::new(), base],
            ..Self::project()
        }
    }
    pub fn load_branches(&mut self, path: &Path) {
        match branch_candidates(path) {
            Ok(branches) => self.branches = branches,
            Err(error) => self.branch_error = Some(format!("{error:#}")),
        }
    }
    pub fn completing_branch(&self) -> bool {
        self.project.is_some()
            && self.deletion.is_none()
            && self.existing.is_none()
            && self.field == 0
            && !self.busy()
    }
    pub fn matching_branches(&self) -> Vec<&BranchCandidate> {
        self.branches
            .iter()
            .filter(|branch| branch.label.contains(&self.fields[0]))
            .collect()
    }
    pub fn complete_branch(&mut self) -> bool {
        if !self.completing_branch() {
            return false;
        }
        let selected = self
            .matching_branches()
            .get(self.branch_selection)
            .map(|branch| (*branch).clone());
        if let Some(branch) = selected {
            self.fields[0] = branch.name.clone();
            self.branch_input = self.fields[0].clone();
            self.branch_selection = self
                .matching_branches()
                .iter()
                .position(|candidate| candidate.label == branch.label)
                .unwrap();
            self.selected_branch = Some(branch);
            self.error = None;
            self.scroll = 0;
            true
        } else {
            false
        }
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn completing_path(&self) -> bool {
        self.project.is_none() && self.field == 0 && !self.busy()
    }
    pub fn delete(
        project: String,
        name: String,
        common: &Path,
        branch: String,
        path: std::path::PathBuf,
    ) -> Result<Self> {
        let expected = serde_json::to_string(&(common, &branch, &path))?;
        let ignored = if path.try_exists()? {
            crate::git::ignored(&path)?
        } else {
            Vec::new()
        };
        Ok(Self {
            project: Some(project),
            fields: [name, String::new()],
            deletion: Some(Deletion {
                branch,
                path,
                expected,
                delete_branch: true,
                delete_ignored: true,
                ignored,
                confirm: false,
                option: 0,
            }),
            ..Self::project()
        })
    }
    pub fn complete_path(&mut self) -> bool {
        if !self.completing_path() {
            return false;
        }
        if let Some(path) = self.directories.get(self.directory_selection) {
            self.fields[0] = path.clone();
            true
        } else {
            false
        }
    }
    pub fn confirm(&mut self, registry: &Path) -> Result<()> {
        self.operation_started = false;
        if self.project.is_none() && self.field == 0 {
            ensure!(!self.fields[0].trim().is_empty(), "请输入仓库路径");
            let mut path = registry::expand(Path::new(&self.fields[0]))?;
            // 已输入的完整目录优先；不能因其有子目录候选而误入下一级。
            if !path.is_dir() && self.complete_path() {
                path = registry::expand(Path::new(&self.fields[0]))?;
            }
            ensure!(path.is_dir(), "目录不存在：{}", path.display());
            ensure!(
                git(&path, &["rev-parse", "--is-bare-repository"])? == "false",
                "暂不支持裸仓库"
            );
            let main = crate::git::worktrees(&path)?
                .first()
                .context("未找到主工作目录")?
                .0
                .clone();
            let alias = main
                .file_name()
                .and_then(|s| s.to_str())
                .context("无法取得仓库目录名")?
                .to_owned();
            if self.fields[1].is_empty() || self.default_alias.as_ref() == Some(&self.fields[1]) {
                self.fields[1] = alias.clone();
            }
            self.default_alias = Some(alias);
            self.field = 1;
            self.error = None;
            self.scroll = 0;
            return Ok(());
        }
        self.complete_branch();
        self.submit(registry)
    }
    fn submit(&mut self, registry: &Path) -> Result<()> {
        ensure!(
            self.deletion.is_some()
                || (!self.fields[0].trim().is_empty() && !self.fields[1].trim().is_empty()),
            "请填写全部字段"
        );
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("--config").arg(registry);
        if let Some(deletion) = &mut self.deletion {
            ensure!(deletion.confirm, "请明确确认删除");
            deletion.confirm = false;
            command.args([
                "rm",
                self.project.as_deref().unwrap(),
                &self.fields[0],
                "--yes",
                "--expected-task",
                &deletion.expected,
            ]);
            if !deletion.delete_branch {
                command.arg("--keep-branch");
            } else {
                command.arg("--console-delete-branch");
            }
            if deletion.delete_ignored {
                command
                    .arg("--delete-ignored")
                    .arg("--expected-ignored")
                    .arg(serde_json::to_string(&deletion.ignored)?);
            }
        } else if let Some(project) = &self.project {
            crate::config::task_name(&self.fields[0])?;
            ensure!(self.fields[0] != "main", "main 保留给项目主窗口");
            let entry = registry::entry(registry, project)?;
            let path = registry::expand(&entry.path)?;
            let repository = crate::config::Project::discover_at(&path, registry)?;
            ensure!(
                !repository.state()?.contains_key(&self.fields[0]),
                "任务 {} 已存在，请从任务列表进入；资源异常时请查看 twig guide troubleshooting",
                self.fields[0]
            );
            if let Some(existing) = self.existing.take() {
                ensure!(existing.confirmed, "操作已取消");
                let expected = serde_json::to_string(&(
                    &existing.common,
                    &existing.branches[existing.selected],
                ))?;
                command.args([
                    "new",
                    project,
                    &self.fields[0],
                    "--expected-branch",
                    &expected,
                ]);
            } else {
                command.args(["new", project, &self.fields[0]]);
                let config = repository.config()?;
                let mut branches = crate::existing_branches(&config, &self.fields[0])?;
                let mut selected = if let Some(candidate) = &self.selected_branch {
                    let reference = candidate.reference();
                    if branches
                        .first()
                        .is_some_and(|branch| branch.reference.starts_with("refs/heads/"))
                    {
                        0 // 保持本地同名优先。
                    } else {
                        branches
                            .iter()
                            .position(|branch| branch.reference == reference)
                            .context("所选分支已不存在，请重新打开表单选择")?
                    }
                } else {
                    0
                };
                if !branches.is_empty() {
                    if self.selected_branch.is_some() {
                        branches = vec![branches.remove(selected)];
                        selected = 0;
                    }
                    self.existing = Some(ExistingBranch {
                        branches,
                        common: repository.common,
                        selected,
                        confirmed: false,
                    });
                    self.error = None;
                    self.scroll = 0;
                    return Ok(());
                }
            }
            command.args(["--base", &self.fields[1], "--no-open"]);
        } else {
            crate::config::name(&self.fields[1])?;
            command.args([
                "add",
                &self.fields[1],
                "--path",
                &self.fields[0],
                "--print-name",
            ]);
        }
        let output = tempfile::tempfile()?;
        let child = command
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output.try_clone()?)
            .spawn()
            .context("无法启动创建操作")?;
        self.job = Some(Job { child, output });
        self.operation_started = true;
        self.error = None;
        self.scroll = 0;
        Ok(())
    }
    // 写操作不使用采集超时，也不强杀，以保留可诊断的部分成功状态。
    pub fn poll(&mut self) -> Result<Option<(String, String)>> {
        if self.branch_input != self.fields[0] {
            self.branch_input = self.fields[0].clone();
            self.branch_selection = 0;
            self.selected_branch = None;
        }
        if self.completing_path() && self.completion_input.as_ref() != Some(&self.fields[0]) {
            self.completion_input = Some(self.fields[0].clone());
            self.directory_selection = 0;
            self.directory_error = None;
            match directories(&self.fields[0]) {
                Ok(paths) => self.directories = paths,
                Err(error) => {
                    self.directories.clear();
                    self.directory_error = Some(format!("{error:#}"));
                }
            }
        }
        let Some(job) = &mut self.job else {
            return Ok(None);
        };
        let Some(status) = job.child.try_wait()? else {
            return Ok(None);
        };
        let mut job = self.job.take().unwrap();
        job.output.rewind()?;
        let mut output = String::new();
        job.output.read_to_string(&mut output)?;
        ensure!(status.success(), "{}", output.trim());
        let selection = if let Some(project) = &self.project {
            (
                project.clone(),
                if self.deletion.is_some() {
                    "main".into()
                } else {
                    self.fields[0].clone()
                },
            )
        } else {
            let name: String =
                serde_json::from_str(output.lines().last().context("登记未返回项目别名")?)?;
            (name, "main".to_owned())
        };
        Ok(Some(selection))
    }
}

fn branch_candidates(path: &Path) -> Result<Vec<BranchCandidate>> {
    let remotes = git(path, &["remote"])?;
    let references = git(
        path,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(symref)",
            "refs/heads/",
            "refs/remotes/",
        ],
    )?;
    let mut candidates = Vec::new();
    for line in references.lines() {
        let mut fields = line.split_whitespace();
        let reference = fields.next().context("分支列表缺少引用名称")?;
        if fields.next().is_some() {
            continue; // 排除 origin/HEAD 等符号引用。
        }
        let (name, label) = if let Some(name) = reference.strip_prefix("refs/heads/") {
            (name, name)
        } else if let Some(label) = reference.strip_prefix("refs/remotes/") {
            let Some(name) = remotes
                .lines()
                .find_map(|remote| label.strip_prefix(&format!("{remote}/")))
            else {
                continue;
            };
            (name, label)
        } else {
            continue;
        };
        if name != "main" {
            candidates.push(BranchCandidate {
                name: name.into(),
                label: label.into(),
            });
        }
    }
    Ok(candidates)
}

fn directories(input: &str) -> Result<Vec<String>> {
    if input == "~" {
        return Ok(vec!["~/".into()]);
    }
    let (parent, prefix) = input
        .rsplit_once('/')
        .map(|(parent, prefix)| (format!("{parent}/"), prefix))
        .unwrap_or_else(|| (String::new(), input));
    let directory = registry::expand(Path::new(if parent.is_empty() { "." } else { &parent }))?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("无法读取目录 {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() || (kind.is_symlink() && entry.path().is_dir()) {
            paths.push(format!("{parent}{name}/"));
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn branch_filter_and_completion_preserve_base_and_handle_no_matches() {
        let mut form = Form::task("demo".into(), "develop".into());
        form.branches = ["feature/alpha", "feature/beta"]
            .into_iter()
            .map(|name| BranchCandidate {
                name: name.into(),
                label: name.into(),
            })
            .collect();
        form.field = 1;
        assert!(!form.complete_branch());
        form.field = 0;
        form.fields[0] = "feature/".into();
        form.poll().unwrap();
        assert_eq!(form.matching_branches().len(), 2);
        form.branch_selection = 1;
        assert!(form.complete_branch());
        assert_eq!(form.fields, ["feature/beta", "develop"]);
        form.poll().unwrap();
        assert_eq!(form.branch_selection, 0);
        form.fields[0] = "new-task".into();
        form.poll().unwrap();
        assert!(form.matching_branches().is_empty());
        assert!(!form.complete_branch());
        form.fields[0].clear();
        form.poll().unwrap();
        assert_eq!(form.matching_branches().len(), 2);
        form.branches.push(BranchCandidate {
            name: "fix/login".into(),
            label: "team/mirror/fix/login".into(),
        });
        form.fields[0] = "team/mirror/".into();
        form.poll().unwrap();
        assert!(form.complete_branch());
        assert_eq!(form.fields, ["fix/login", "develop"]);
    }

    #[test]
    fn directory_completion_preserves_paths_and_only_lists_matching_directories() {
        let temp = tempfile::tempdir().unwrap();
        for name in [
            "repo with spaces",
            "repo-two",
            ".hidden",
            "repo with spaces/child",
        ] {
            std::fs::create_dir_all(temp.path().join(name)).unwrap();
        }
        std::fs::write(temp.path().join("repo-file"), "untouched").unwrap();
        std::os::unix::fs::symlink(temp.path().join("repo-two"), temp.path().join("repo-link"))
            .unwrap();
        let prefix = format!("{}/repo", temp.path().display());
        let paths = directories(&prefix).unwrap();
        assert_eq!(
            paths,
            ["repo with spaces/", "repo-link/", "repo-two/"]
                .map(|name| format!("{}/{name}", temp.path().display()))
        );
        assert_eq!(
            directories(&format!("{}/.", temp.path().display())).unwrap(),
            vec![format!("{}/.hidden/", temp.path().display())]
        );
        assert_eq!(directories("~").unwrap(), vec!["~/"]);
        assert!(directories(&format!("{}/absent/", temp.path().display())).is_err());
        let mut form = Form::project();
        form.fields[0] = prefix;
        form.poll().unwrap();
        assert!(form.complete_path());
        assert!(form.fields[0].ends_with("repo with spaces/"));
        form.poll().unwrap();
        assert!(form.complete_path());
        assert!(form.fields[0].ends_with("repo with spaces/child/"));
        form.poll().unwrap();
        assert!(!form.complete_path());
        form.field = 1;
        assert!(!form.complete_path());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("repo-file")).unwrap(),
            "untouched"
        );
    }
}
