use twig_cli::notification::{PickerAction, UnreadPicker};
mod data;
pub(crate) mod form;
mod notifications;
pub(crate) mod query;
mod selection;
mod ui;

use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableMouseCapture, Event, KeyCode,
        KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use data::{Request, Snapshot, Worker};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{
    io::{self, IsTerminal},
    path::PathBuf,
    time::{Duration, Instant},
};

pub(super) struct App {
    snapshot: Snapshot,
    notifications: notifications::Notifications,
    project: Option<String>,
    task: Option<PathBuf>,
    thread: Option<String>,
    focus: usize,
    filters: [String; 3],
    searching: bool,
    search_input: String,
    command_selection: Option<usize>,
    search_backup: Option<SearchBackup>,
    help: bool,
    history: bool,
    descendants: bool,
    page: u64,
    refresh: u64,
    detail: bool,
    metadata: bool,
    scroll: u16,
    message: Option<String>,
    initial_cwd: PathBuf,
    located: bool,
    form: Option<form::Form>,
    pending_selection: Option<(String, String)>,
    pending_removed: Option<String>,
    pane_prefix: bool,
    top_prefix: bool,
    delete_prefix: bool,
}
struct SearchBackup {
    filters: [String; 3],
    project: Option<String>,
    task: Option<PathBuf>,
    thread: Option<String>,
    history: bool,
    page: u64,
    detail: bool,
    scroll: u16,
}
impl App {
    fn new(cwd: PathBuf) -> Self {
        Self {
            notifications: Default::default(),
            snapshot: Snapshot {
                codex_error: Some("正在连接 Codex…".into()),
                ..Default::default()
            },
            project: None,
            task: None,
            thread: None,
            focus: 0,
            filters: Default::default(),
            searching: false,
            search_input: String::new(),
            command_selection: None,
            search_backup: None,
            help: false,
            history: false,
            descendants: false,
            page: 0,
            refresh: 0,
            detail: false,
            metadata: false,
            scroll: 0,
            message: None,
            initial_cwd: cwd,
            located: false,
            form: None,
            pending_selection: None,
            pending_removed: None,
            pane_prefix: false,
            top_prefix: false,
            delete_prefix: false,
        }
    }
    fn receive_notifications(&mut self, worker: &Worker, viewed: Option<(&str, &std::path::Path)>) {
        for completion in worker.completions.try_iter() {
            self.notifications.receive(completion, viewed);
        }
        self.notifications.tick(Instant::now());
    }

    fn begin_search(&mut self) {
        self.search_backup = Some(SearchBackup {
            filters: self.filters.clone(),
            project: self.project.clone(),
            task: self.task.clone(),
            thread: self.thread.clone(),
            history: self.history,
            page: self.page,
            detail: self.detail,
            scroll: self.scroll,
        });
        self.search_input.clear();
        self.command_selection = None;
        self.searching = true;
        self.preview_search();
    }
    fn command_candidates(&self) -> Vec<&'static str> {
        if self.search_input.starts_with('/') {
            return vec![];
        }
        ["new", "delete", "refresh", "help", "quit"]
            .into_iter()
            .filter(|name| name.starts_with(&self.search_input))
            .collect()
    }
    fn preview_search(&mut self) {
        self.filters[self.focus] = self
            .search_input
            .strip_prefix('/')
            .unwrap_or(&self.search_input)
            .to_owned();
        self.reconcile();
    }
    fn restore_search(&mut self) {
        if let Some(previous) = self.search_backup.take() {
            self.filters = previous.filters;
            self.project = previous.project;
            self.task = previous.task;
            self.thread = previous.thread;
            self.history = previous.history;
            self.page = previous.page;
            self.detail = previous.detail;
            self.scroll = previous.scroll;
            self.reconcile();
        }
        self.searching = false;
        self.command_selection = None;
    }
    fn new_form(&mut self) {
        match self.focus {
            0 => self.form = Some(form::Form::project()),
            1 => {
                if let Some(repo) = self.repo() {
                    if let Some((_, config)) = &repo.context {
                        self.form = Some(form::Form::task(repo.name.clone(), config.base.clone()));
                    } else {
                        self.message = Some("项目不可用，请先处理项目异常".into());
                    }
                } else {
                    self.message = Some("请先选择项目".into());
                }
            }
            _ => self.message = Some("/new：请先切到项目区或任务区".into()),
        }
    }
    fn search_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        let candidates = self.command_candidates();
        match key.code {
            KeyCode::Esc => self.restore_search(),
            KeyCode::Down | KeyCode::Up if !candidates.is_empty() => {
                self.command_selection = Some(match self.command_selection {
                    None if key.code == KeyCode::Up => candidates.len() - 1,
                    None => 0,
                    Some(i) if key.code == KeyCode::Up => {
                        (i + candidates.len() - 1) % candidates.len()
                    }
                    Some(i) => (i + 1) % candidates.len(),
                });
            }
            KeyCode::Tab if !candidates.is_empty() => {
                self.search_input = candidates[self.command_selection.unwrap_or(0)].into();
                self.command_selection = Some(0);
                self.preview_search();
            }
            KeyCode::Enter => {
                let command = self
                    .command_selection
                    .and_then(|i| candidates.get(i).copied())
                    .or_else(|| {
                        candidates
                            .iter()
                            .copied()
                            .find(|name| *name == self.search_input)
                    });
                if let Some(command) = command {
                    self.restore_search();
                    match command {
                        "new" => self.new_form(),
                        "delete" => self.delete_form(),
                        "refresh" => {
                            self.refresh += 1;
                            self.message = None;
                        }
                        "help" => self.help = true,
                        "quit" => return true,
                        _ => unreachable!(),
                    }
                } else {
                    self.searching = false;
                    self.search_backup = None;
                    self.command_selection = None;
                }
            }
            KeyCode::Backspace => {
                self.search_input.pop();
                self.command_selection = None;
                self.preview_search();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.push(c);
                self.command_selection = None;
                self.preview_search();
            }
            _ => {}
        }
        false
    }
    fn delete_form(&mut self) {
        let result = (|| -> Result<form::Form> {
            ensure!(self.focus == 1, "/delete：请先切到任务区");
            let repo = self.repo().ok_or_else(|| anyhow::anyhow!("请先选择项目"))?;
            let task = self.task().ok_or_else(|| anyhow::anyhow!("请先选择任务"))?;
            ensure!(task.name != "main", "不能删除主工作目录");
            let (project, _) = repo
                .context
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("项目不可用"))?;
            form::Form::delete(
                repo.name.clone(),
                task.name.clone(),
                &project.common,
                task.branch.clone(),
                task.path.clone(),
            )
        })();
        match result {
            Ok(form) => self.form = Some(form),
            Err(error) => self.message = Some(format!("{error:#}")),
        }
    }
    fn repos(&self) -> Vec<&data::Repo> {
        let filter = self.filters[0].to_lowercase();
        self.snapshot
            .repos
            .iter()
            .filter(|r| r.name.to_lowercase().contains(&filter))
            .collect()
    }
    fn repo(&self) -> Option<&data::Repo> {
        self.snapshot
            .repos
            .iter()
            .find(|r| Some(&r.name) == self.project.as_ref())
    }
    fn tasks(&self) -> Vec<&data::Task> {
        let filter = self.filters[1].to_lowercase();
        self.repo()
            .map(|r| {
                r.tasks
                    .iter()
                    .filter(|t| {
                        format!("{} {}", t.name, t.branch)
                            .to_lowercase()
                            .contains(&filter)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn task(&self) -> Option<&data::Task> {
        self.repo()?
            .tasks
            .iter()
            .find(|t| Some(&t.path) == self.task.as_ref())
    }
    fn threads(&self) -> Vec<&crate::codex::Thread> {
        let (Some(project), Some(path)) = (&self.project, &self.task) else {
            return vec![];
        };
        let mut threads = data::for_task(&self.snapshot.threads, &self.snapshot, project, path);
        if self.history
            && self.snapshot.history_key.as_ref() == Some(&(path.clone(), self.descendants))
        {
            for thread in &self.snapshot.history {
                if !threads.iter().any(|t| t.id == thread.id) {
                    threads.push(thread);
                }
            }
        }
        let filter = self.filters[2].to_lowercase();
        threads.retain(|t| {
            format!("{} {}", t.title(), t.id)
                .to_lowercase()
                .contains(&filter)
        });
        threads.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id.cmp(&b.id)));
        threads
    }
    fn reconcile(&mut self) {
        if let Some((project, task)) = &self.pending_selection
            && let Some(repo) = self.snapshot.repos.iter().find(|r| &r.name == project)
            && !repo
                .tasks
                .iter()
                .any(|task| self.pending_removed.as_ref() == Some(&task.name))
            && let Some(task) = repo.tasks.iter().find(|t| &t.name == task)
        {
            self.project = Some(project.clone());
            self.task = Some(task.path.clone());
            self.focus = 1;
            self.filters = Default::default();
            self.detail = false;
            self.history = false;
            self.located = true;
            self.message = Some(format!("操作成功，已选中 {project} / {}", task.name));
            self.pending_selection = None;
            self.pending_removed = None;
        }
        if !self.located && !self.snapshot.repos.is_empty() {
            if let Some((repo, task)) = self
                .snapshot
                .repos
                .iter()
                .flat_map(|r| r.tasks.iter().map(move |t| (r, t)))
                .filter(|(_, t)| self.initial_cwd.starts_with(&t.path))
                .max_by_key(|(_, t)| t.path.components().count())
            {
                self.project = Some(repo.name.clone());
                self.task = Some(task.path.clone());
            }
            self.located = true;
        }
        let repos = self.repos();
        if !repos.iter().any(|r| Some(&r.name) == self.project.as_ref()) {
            self.project = repos.first().map(|r| r.name.clone());
        }
        let tasks = self.tasks();
        if !tasks.iter().any(|t| Some(&t.path) == self.task.as_ref()) {
            self.task = tasks.first().map(|t| t.path.clone());
        }
        let threads = self.threads();
        if !threads.iter().any(|t| Some(&t.id) == self.thread.as_ref()) {
            self.thread = threads.first().map(|t| t.id.clone());
        }
    }
    fn step(&mut self, direction: isize) {
        self.scroll = 0;
        match self.focus {
            0 => {
                let ids: Vec<_> = self.repos().iter().map(|r| r.name.clone()).collect();
                self.project = select(&ids, self.project.as_ref(), direction);
                self.history = false;
                self.page = 0;
                self.detail = false;
            }
            1 => {
                let ids: Vec<_> = self.tasks().iter().map(|t| t.path.clone()).collect();
                self.task = select(&ids, self.task.as_ref(), direction);
                self.history = false;
                self.page = 0;
                self.detail = false;
            }
            _ => {
                let ids: Vec<_> = self.threads().iter().map(|t| t.id.clone()).collect();
                self.thread = select(&ids, self.thread.as_ref(), direction);
                self.detail = false;
            }
        }
        self.reconcile();
    }
    fn request(&self) -> Request {
        Request {
            project: self.project.clone(),
            path: self.task.clone(),
            history: self.history,
            descendants: self.descendants,
            page: self.page,
            turn: self.thread.clone().filter(|_| self.detail),
            refresh: self.refresh,
        }
    }
}
fn select<T: PartialEq + Clone>(items: &[T], selected: Option<&T>, direction: isize) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let index = items.iter().position(|i| Some(i) == selected).unwrap_or(0);
    Some(items[index.saturating_add_signed(direction).min(items.len() - 1)].clone())
}

struct Screen {
    active: bool,
}
impl Screen {
    fn start() -> Result<Self> {
        enable_raw_mode()?;
        let mut screen = Self { active: true };
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture) {
            screen.restore()?;
            return Err(e.into());
        }
        Ok(screen)
    }
    fn restore(&mut self) -> Result<()> {
        if self.active {
            // 两项都尝试，避免其中一项失败跳过另一项。
            let raw = disable_raw_mode();
            let screen = execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen,
                crossterm::cursor::Show
            );
            self.active = false;
            raw?;
            screen?;
        }
        Ok(())
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub struct TerminalTarget {
    pub project: String,
    pub session: String,
    pub window: String,
}

fn notification_target(
    app: &mut App,
    registry: &std::path::Path,
    notice: &twig_cli::notification::CompletionNotice,
) -> Result<TerminalTarget> {
    let path = app
        .snapshot
        .repos
        .iter()
        .find(|repo| repo.name == notice.project)
        .and_then(|repo| repo.tasks.iter().find(|task| task.name == notice.task))
        .map(|task| task.path.clone())
        .context("通知对应的项目或任务已不可用")?;
    let (session, window) = data::prepare_terminal(registry, &notice.project, &notice.task)?;
    app.project = Some(notice.project.clone());
    app.task = Some(path);
    app.filters = Default::default();
    app.detail = false;
    app.history = false;
    app.scroll = 0;
    app.located = true;
    Ok(TerminalTarget {
        project: notice.project.clone(),
        session,
        window,
    })
}

fn open_terminal(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    worker: &Worker,
    mut target: TerminalTarget,
    registry: &std::path::Path,
    mut jump: Option<twig_cli::notification::CompletionNotice>,
) -> Result<()> {
    let result = loop {
        let initial_task = app.task.clone();
        let result =
            twig_cli::workbench::run(terminal, target.session, &target.window, |window, task| {
                if let Some(notice) = jump.take() {
                    app.notifications.dismiss(&notice);
                }
                for snapshot in worker.updates.try_iter() {
                    app.snapshot = snapshot;
                }
                let viewed = app
                    .snapshot
                    .repos
                    .iter()
                    .find(|r| r.name == target.project)
                    .and_then(|r| r.tasks.iter().find(|t| t.name == task))
                    .map(|t| t.path.clone())
                    .or_else(|| {
                        (window == target.window)
                            .then(|| initial_task.clone())
                            .flatten()
                    });
                app.receive_notifications(
                    worker,
                    viewed
                        .as_deref()
                        .map(|path| (target.project.as_str(), path)),
                );
                if let Some(path) = viewed {
                    app.notifications.read(&target.project, &path);
                }
                let summary = data::summary(app.snapshot.threads.iter());
                let text = format!(
                    "{} · 全局 Codex {}",
                    target.project,
                    if app.snapshot.codex_error.is_some() {
                        "连接异常"
                    } else if app
                        .snapshot
                        .codex_checked
                        .is_none_or(|checked| checked.elapsed() > Duration::from_secs(30))
                    {
                        "状态过期"
                    } else {
                        &summary
                    }
                );
                twig_cli::workbench::Status {
                    text,
                    notification: app.notifications.current().cloned(),
                    unread: app.notifications.tasks(),
                }
            });
        match result {
            Ok(Some(notice)) => match notification_target(app, registry, &notice) {
                Ok(next) => {
                    target = next;
                    jump = Some(notice);
                }
                Err(error) => break Err(error),
            },
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    app.refresh += 1;
    app.reconcile();
    app.focus = 0;
    app.message = result
        .err()
        .map(|error| format!("终端已返回导航：{error:#}"));
    execute!(io::stdout(), EnableMouseCapture)?;
    terminal.clear()?;
    Ok(())
}

pub fn run(registry: PathBuf, cwd: PathBuf, initial: Option<TerminalTarget>) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "Console 工作台需要交互式终端；已准备的环境保留，脚本创建请用 new --no-open，查询请用 twig ls"
    );
    // panic 时先恢复终端，再输出诊断；退出后还原原 hook。
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        eprintln!("Console 异常：{info}");
    }));
    let result = run_inner(registry, cwd, initial);
    std::panic::set_hook(previous);
    result
}
fn run_inner(registry: PathBuf, cwd: PathBuf, initial: Option<TerminalTarget>) -> Result<()> {
    let mut screen = Screen::start()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let worker = Worker::start(registry.clone());
    let mut app = App::new(cwd);
    if let Some(target) = &initial {
        app.project = Some(target.project.clone());
        app.task = Some(app.initial_cwd.clone());
        app.focus = 1;
    }
    worker.requests.send(app.request())?;
    if let Some(target) = initial {
        open_terminal(&mut app, &mut terminal, &worker, target, &registry, None)?;
        worker.requests.send(app.request())?;
    }
    let mut selection: Option<selection::Selection> = None;
    let mut unread_picker: Option<UnreadPicker> = None;
    let mut copy_notice: Option<twig_cli::clipboard::Notice> = None;
    loop {
        if let Some(form) = &mut app.form {
            match form.poll() {
                Ok(Some(selection)) => {
                    app.pending_removed = form.deletion.as_ref().map(|_| form.fields[0].clone());
                    app.pending_selection = Some(selection);
                    app.form = None;
                    app.refresh += 1;
                    app.message = Some(if app.pending_removed.is_some() {
                        "删除已完成，正在刷新项目与任务…".into()
                    } else {
                        "操作成功，正在刷新项目与任务…".into()
                    });
                    worker.requests.send(app.request())?;
                }
                Err(error) => {
                    form.error = Some(format!("{error:#}"));
                    form.scroll = 0;
                }
                Ok(None) => {}
            }
        }
        let mut updated = false;
        for snapshot in worker.updates.try_iter() {
            app.snapshot = snapshot;
            updated = true;
        }
        if updated {
            app.reconcile();
            worker.requests.send(app.request())?;
        }
        app.receive_notifications(&worker, None);
        let unread_tasks = app.notifications.tasks();
        if let Some(picker) = &mut unread_picker {
            picker.sync(&unread_tasks);
        }
        if copy_notice
            .as_ref()
            .is_some_and(twig_cli::clipboard::Notice::expired)
        {
            copy_notice = None;
        }
        let mut rendered = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::default());
        let mut regions = Vec::new();
        terminal.draw(|frame| {
            if selection
                .as_ref()
                .is_some_and(|selection| !selection.fits(frame.area()))
            {
                selection = None;
            }
            ui::draw(frame, &app);
            rendered = frame.buffer_mut().clone();
            regions = ui::selection_regions(frame.area(), &app);
            if let Some(selection) = &selection {
                selection.draw(frame.buffer_mut());
            }
            if let Some(notice) = app.notifications.current() {
                twig_cli::notification::draw(frame, notice);
            }
            if let Some(notice) = &copy_notice {
                notice.draw(frame);
            }
            if let Some(picker) = &unread_picker {
                picker.draw(frame, &unread_tasks);
            }
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let event = event::read()?;
        if let Some(picker) = &mut unread_picker {
            if let Event::Key(key) = event {
                match picker.key(key, &unread_tasks) {
                    PickerAction::Stay => {}
                    PickerAction::Close => unread_picker = None,
                    PickerAction::Open(notice) => {
                        unread_picker = None;
                        match notification_target(&mut app, &registry, &notice) {
                            Ok(target) => open_terminal(
                                &mut app,
                                &mut terminal,
                                &worker,
                                target,
                                &registry,
                                Some(notice),
                            )?,
                            Err(error) => app.message = Some(format!("跳转失败：{error:#}")),
                        }
                        worker.requests.send(app.request())?;
                    }
                }
            }
            continue;
        }
        match event {
            Event::Resize(_, _) => {
                selection = None;
                continue;
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        selection = regions
                            .iter()
                            .find(|area| area.contains((mouse.column, mouse.row).into()))
                            .map(|area| {
                                selection::Selection::new(&rendered, *area, mouse.column, mouse.row)
                            });
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(selection) = &mut selection
                            && selection.dragging
                        {
                            selection.update(mouse.column, mouse.row);
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        if let Some(selected) = &mut selection
                            && selected.dragging
                        {
                            selected.update(mouse.column, mouse.row);
                            selected.dragging = false;
                            let text = selected.text();
                            if text.is_empty() {
                                selection = None;
                            } else {
                                let message = twig_cli::clipboard::copy(&text)
                                    .unwrap_or_else(|error| format!("复制失败：{error}"));
                                copy_notice = Some(twig_cli::clipboard::Notice::new(message));
                            }
                        }
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => selection = None,
                    _ => {}
                }
                continue;
            }
            _ => {}
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if selection.take().is_some() && key.code == KeyCode::Esc {
            continue;
        }
        if let Some(form) = &mut app.form {
            if key.code == KeyCode::PageDown {
                form.scroll = form.scroll.saturating_add(2);
                continue;
            }
            if key.code == KeyCode::PageUp {
                form.scroll = form.scroll.saturating_sub(2);
                continue;
            }
            if form.busy() {
                continue;
            }
            if let Some(existing) = &mut form.existing {
                match key.code {
                    KeyCode::Esc => form.existing = None,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.form = None
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                        existing.confirmed = !existing.confirmed
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        existing.selected = existing.selected.saturating_sub(1);
                        existing.confirmed = false;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        existing.selected =
                            (existing.selected + 1).min(existing.branches.len() - 1);
                        existing.confirmed = false;
                    }
                    KeyCode::Enter if !existing.confirmed => form.existing = None,
                    KeyCode::Enter => {
                        if let Err(error) = form.confirm(&registry) {
                            form.error = Some(format!("{error:#}"));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if let Some(deletion) = &mut form.deletion {
                match key.code {
                    KeyCode::Esc => app.form = None,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.form = None
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                        deletion.confirm = !deletion.confirm
                    }
                    KeyCode::Up | KeyCode::Char('k') => deletion.option = 0,
                    KeyCode::Down | KeyCode::Char('j') => deletion.option = 1,
                    KeyCode::Char(' ') => {
                        if deletion.option == 0 {
                            deletion.delete_branch = !deletion.delete_branch;
                        } else {
                            deletion.delete_ignored = !deletion.delete_ignored;
                        }
                        deletion.confirm = false;
                    }
                    KeyCode::Enter if !deletion.confirm => app.form = None,
                    KeyCode::Enter => {
                        if let Err(error) = form.confirm(&registry) {
                            form.error = Some(format!("{error:#}"));
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => app.form = None,
                KeyCode::Tab => {
                    if !form.complete_path() {
                        form.field = (form.field + 1) % 2;
                    }
                }
                KeyCode::BackTab => form.field = (form.field + 1) % 2,
                KeyCode::Down if form.completing_path() && !form.directories.is_empty() => {
                    form.directory_selection =
                        (form.directory_selection + 1) % form.directories.len();
                }
                KeyCode::Up if form.completing_path() && !form.directories.is_empty() => {
                    form.directory_selection = (form.directory_selection + form.directories.len()
                        - 1)
                        % form.directories.len();
                }
                KeyCode::Backspace => {
                    form.fields[form.field].pop();
                    form.error = None;
                    form.scroll = 0;
                }
                KeyCode::Enter => {
                    if let Err(error) = form.confirm(&registry) {
                        form.error = Some(format!("{error:#}"));
                    }
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.form = None
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.fields[form.field].push(c);
                    form.error = None;
                    form.scroll = 0;
                }
                _ => {}
            }
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
        if app.help {
            app.help = false;
            continue;
        }
        if app.searching {
            if app.search_key(key) {
                break;
            }
            worker.requests.send(app.request())?;
            continue;
        }
        if app.pane_prefix {
            app.pane_prefix = false;
            if key.code == KeyCode::Char('u') {
                unread_picker = Some(UnreadPicker::default());
                selection = None;
            } else if key.code == KeyCode::Char('f') {
                if let Some(notice) = app.notifications.current().cloned() {
                    match notification_target(&mut app, &registry, &notice) {
                        Ok(target) => open_terminal(
                            &mut app,
                            &mut terminal,
                            &worker,
                            target,
                            &registry,
                            Some(notice),
                        )?,
                        Err(error) => app.message = Some(format!("跳转失败：{error:#}")),
                    }
                }
            } else if let KeyCode::Char(direction) = key.code {
                app.focus = match (app.focus, direction.to_ascii_lowercase()) {
                    (_, 'h') => 0,
                    (0, 'l') => 1,
                    (1, 'j') => 2,
                    (2, 'k') => 1,
                    _ => app.focus,
                };
            }
            worker.requests.send(app.request())?;
            continue;
        }
        if app.delete_prefix {
            app.delete_prefix = false;
            if key.code == KeyCode::Char('d') && key.modifiers.is_empty() {
                app.delete_form();
                continue;
            }
            if key.code == KeyCode::Esc {
                continue;
            }
        }
        if app.top_prefix {
            app.top_prefix = false;
            if key.code == KeyCode::Char('g') && !key.modifiers.contains(KeyModifiers::CONTROL) {
                app.step(isize::MIN);
                worker.requests.send(app.request())?;
                continue;
            }
            if key.code == KeyCode::Esc {
                continue;
            }
        }
        if matches!(key.code, KeyCode::Char('w' | 'W'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            app.pane_prefix = true;
            continue;
        }
        match key.code {
            KeyCode::Char('d') if app.focus == 1 && key.modifiers.is_empty() => {
                app.delete_prefix = true;
            }
            KeyCode::Char('q') => break,
            KeyCode::Char('?') => app.help = true,
            KeyCode::Char('i') => {
                app.metadata = !app.metadata;
                app.scroll = 0;
            }
            KeyCode::PageDown => app.scroll = app.scroll.saturating_add(4),
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(4),
            KeyCode::Tab => app.focus = (app.focus + 1) % 3,
            KeyCode::BackTab => app.focus = (app.focus + 2) % 3,
            KeyCode::Down | KeyCode::Char('j') => app.step(1),
            KeyCode::Up | KeyCode::Char('k') => app.step(-1),
            KeyCode::Char('g') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.top_prefix = true
            }
            KeyCode::Char('G') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.step(isize::MAX)
            }
            KeyCode::Char('/') => app.begin_search(),
            KeyCode::Esc => {
                app.filters[app.focus].clear();
                if app.detail {
                    app.detail = false;
                } else {
                    app.focus = app.focus.saturating_sub(1);
                }
                app.reconcile();
            }
            KeyCode::Char('r') => {
                app.refresh += 1;
                app.message = None;
            }
            KeyCode::Char('h') => {
                app.history = !app.history;
                app.page = 0;
                app.detail = false;
                app.focus = 2;
            }
            KeyCode::Char('a') if app.history => {
                app.descendants = !app.descendants;
                app.page = 0;
            }
            KeyCode::Char('n') if app.focus < 2 => app.new_form(),
            KeyCode::Char('n') if app.focus == 2 && app.history => {
                if app.snapshot.history_more && app.snapshot.history_pages > app.page {
                    app.page += 1;
                }
            }
            KeyCode::Enter if app.focus == 2 => app.detail = true,
            KeyCode::Enter if app.focus == 0 => {
                if app.repo().is_some() {
                    app.filters[1].clear();
                    app.task = app.repo().and_then(|r| {
                        r.tasks
                            .iter()
                            .find(|t| t.name == "main")
                            .map(|t| t.path.clone())
                    });
                    app.focus = 1;
                    app.detail = false;
                    app.scroll = 0;
                    app.reconcile();
                }
            }
            KeyCode::Enter => {
                let target = app
                    .repo()
                    .map(|r| (r.name.clone(), app.task().map(|t| t.name.clone())));
                if let Some((project, Some(task))) = target {
                    match data::prepare_terminal(&registry, &project, &task) {
                        Ok((session, window)) => open_terminal(
                            &mut app,
                            &mut terminal,
                            &worker,
                            TerminalTarget {
                                project,
                                session,
                                window,
                            },
                            &registry,
                            None,
                        )?,
                        Err(error) => app.message = Some(format!("进入失败：{error:#}")),
                    }
                }
            }
            _ => {}
        }
        worker.requests.send(app.request())?;
    }
    screen.restore()?;
    drop(worker);
    Ok(())
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use crossterm::event::KeyEvent;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            assert!(!app.search_key(key(KeyCode::Char(c))));
        }
    }
    fn app() -> App {
        let mut app = App::new(PathBuf::from("/demo"));
        app.snapshot.repos.push(data::Repo {
            name: "demo".into(),
            path: "/demo".into(),
            error: None,
            context: Some((
                crate::config::Project {
                    common: "/demo/.git".into(),
                    dir: "/global/projects/demo-id".into(),
                    registry: "/global/config.toml".into(),
                },
                crate::config::Config {
                    version: 1,
                    main: "/demo".into(),
                    root: "/tasks".into(),
                    base: "main".into(),
                    target: "main".into(),
                    session: "test".into(),
                },
            )),
            tasks: ["main", "auth"]
                .into_iter()
                .map(|name| data::Task {
                    name: name.into(),
                    path: PathBuf::from(format!("/tasks/{name}")),
                    branch: name.into(),
                    git: String::new(),
                    window: String::new(),
                    error: None,
                    checked: None,
                })
                .collect(),
        });
        app.reconcile();
        app
    }
    #[test]
    fn commands_restore_context_and_search_is_default() {
        let mut app = app();
        app.focus = 1;
        app.filters[1] = "auth".into();
        app.reconcile();
        let task = app.task.clone();
        app.begin_search();
        assert_eq!(app.command_selection, None);
        type_text(&mut app, "new");
        assert!(app.task.is_none());
        assert!(!app.search_key(key(KeyCode::Enter)));
        assert_eq!(app.task, task);
        assert_eq!(app.filters[1], "auth");
        let form = app.form.take().unwrap();
        assert_eq!(form.project.as_deref(), Some("demo"));
        assert_eq!(form.fields[1], "main");

        app.begin_search();
        type_text(&mut app, "ne");
        app.search_key(key(KeyCode::Enter));
        assert!(app.form.is_none());
        assert_eq!(app.filters[1], "ne");
        app.begin_search();
        type_text(&mut app, "/new");
        assert!(app.command_candidates().is_empty());
        app.search_key(key(KeyCode::Down));
        app.search_key(key(KeyCode::Tab));
        app.search_key(key(KeyCode::Enter));
        assert!(app.form.is_none());
        assert_eq!(app.filters[1], "new");
    }
    #[test]
    fn command_selection_completion_and_cancel_preserve_project() {
        let mut app = app();
        let original = app.project.clone();
        app.begin_search();
        type_text(&mut app, "absent");
        assert!(app.project.is_none());
        app.search_key(key(KeyCode::Esc));
        assert_eq!(app.project, original);
        assert_eq!(app.filters[0], "");
        app.begin_search();
        app.search_key(key(KeyCode::Down));
        app.search_key(key(KeyCode::Enter));
        assert!(app.form.take().is_some());
        app.begin_search();
        type_text(&mut app, "he");
        app.search_key(key(KeyCode::Tab));
        assert_eq!(app.search_input, "help");
        app.search_key(key(KeyCode::Enter));
        assert!(app.help);
        app.help = false;
        assert_eq!(app.project, original);
        app.begin_search();
        type_text(&mut app, "refresh");
        app.search_key(key(KeyCode::Enter));
        assert_eq!(app.refresh, 1);
        app.begin_search();
        app.search_key(key(KeyCode::Up));
        assert!(app.search_key(key(KeyCode::Enter)));
    }
}
