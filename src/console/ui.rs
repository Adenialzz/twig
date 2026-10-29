use super::{App, data};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
};
use std::time::{Duration, Instant};

fn safe(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}
fn is_stale(checked: Option<Instant>) -> bool {
    checked.is_some_and(|at| at.elapsed() >= Duration::from_secs(30))
}
fn beijing_time(timestamp: i64) -> String {
    let Some(shifted) = timestamp.checked_add(8 * 60 * 60) else {
        return "时间无效".into();
    };
    let mut date = std::mem::MaybeUninit::<libc::tm>::uninit();
    // gmtime_r 只写入独立输出，不读取或修改进程时区；先偏移到 UTC+8。
    let result = unsafe { libc::gmtime_r(&shifted, date.as_mut_ptr()) };
    if result.is_null() {
        return "时间无效".into();
    }
    // gmtime_r 成功后所有日期字段均已初始化。
    let date = unsafe { date.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        i64::from(date.tm_year) + 1900,
        date.tm_mon + 1,
        date.tm_mday,
        date.tm_hour,
        date.tm_min,
        date.tm_sec
    )
}
fn block(title: String, focused: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
}
fn list(
    frame: &mut Frame,
    area: Rect,
    title: String,
    items: Vec<Line<'static>>,
    selected: Option<usize>,
    focused: bool,
) {
    let widget = List::new(items)
        .block(block(title, focused))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(widget, area, &mut state);
}
fn panels(area: Rect, searching: bool) -> [Rect; 4] {
    let rows = Layout::vertical([
        Constraint::Length(crate::tabs::HEADER_HEIGHT),
        Constraint::Min(6),
        Constraint::Length(if searching { 2 } else { 1 }),
    ])
    .split(area);
    if area.width < 100 {
        let parts = Layout::vertical([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(rows[1]);
        return [parts[0], parts[0], parts[0], parts[1]];
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).split(rows[1]);
    let left = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(columns[0]);
    let right = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(columns[1]);
    [left[0], right[0], right[1], left[1]]
}

pub fn selection_regions(area: Rect, app: &App) -> Vec<Rect> {
    if area.width < 38 || area.height < 12 {
        return Vec::new();
    }
    if let Some(form) = &app.form {
        return vec![form_area(area, form).inner(ratatui::layout::Margin::new(1, 1))];
    }
    if app.help || app.searching {
        return Vec::new();
    }
    let areas = panels(area, app.searching);
    let mut regions = Vec::new();
    for (index, panel) in areas.into_iter().enumerate() {
        if area.width < 100 && index < 3 && index != app.focus {
            continue;
        }
        regions.push(panel);
    }
    let mut regions: Vec<_> = regions
        .into_iter()
        .map(|area| area.inner(ratatui::layout::Margin::new(1, 1)))
        .filter(|area| area.width > 0 && area.height > 0)
        .collect();
    regions.push(Rect::new(area.x, area.bottom() - 1, area.width, 1));
    regions
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 38 || area.height < 12 {
        frame.render_widget(Paragraph::new("终端过小，请扩大到至少 38×12；q 退出"), area);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(crate::tabs::HEADER_HEIGHT),
            Constraint::Min(6),
            Constraint::Length(if app.searching { 2 } else { 1 }),
        ])
        .split(area);
    let count: usize = app
        .snapshot
        .repos
        .iter()
        .map(|r| r.tasks.len().saturating_sub(1))
        .sum();
    let summary = data::summary(
        app.snapshot
            .threads
            .iter()
            .filter(|t| app.snapshot.associations.contains_key(&t.id)),
    );
    let stale = if app.snapshot.codex_error.is_some() || is_stale(app.snapshot.codex_checked) {
        " [Codex 未知 / 旧值过期]"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!(
            " twig  |  {} 个项目 · {count} 个任务  |  {summary}{stale}",
            app.snapshot.repos.len()
        )),
        Rect::new(
            rows[0].x,
            rows[0].y + crate::tabs::HEADER_HEIGHT - 1,
            rows[0].width,
            1,
        ),
    );
    crate::tabs::draw(
        frame,
        rows[0],
        crate::tabs::Tab::Console,
        "Tab 切换标签",
        app.chat_status.0,
    );
    let areas = panels(area, app.searching);
    if area.width >= 100 {
        projects(frame, areas[0], app);
        tasks(frame, areas[1], app);
        sessions(frame, areas[2], app);
    } else {
        match app.focus {
            0 => projects(frame, areas[0], app),
            1 => tasks(frame, areas[1], app),
            _ => sessions(frame, areas[2], app),
        }
    }
    monitor(frame, areas[3], app);
    let message = app
        .message
        .as_deref()
        .or(app.notification_error.as_deref())
        .or(app.snapshot.registry_error.as_deref())
        .or(app.snapshot.delivery_error.as_deref())
        .or(app.snapshot.codex_error.as_deref());
    let connection = message.map(safe).unwrap_or_else(|| {
        match app.snapshot.codex_checked {
            None => "正在读取 Codex 状态…",
            Some(at) if is_stale(Some(at)) => "Codex 状态过期 · 超过 30 秒未更新",
            Some(_) => "Codex 已连接 · 仅当前服务可见会话",
        }
        .into()
    });
    let mut status = vec![Line::from(connection)];
    if app.searching {
        status.push(Line::from(format!("搜索：/{}", safe(&app.search_input))));
    }
    frame.render_widget(Paragraph::new(status).wrap(Wrap { trim: false }), rows[2]);
    if app.searching {
        let candidates = app.command_candidates();
        if !candidates.is_empty() {
            let height = candidates.len() as u16 + 2;
            let popup = Rect::new(
                area.x,
                rows[2].y.saturating_sub(height),
                area.width.min(72),
                height,
            );
            frame.render_widget(Clear, popup);
            let items = candidates
                .iter()
                .map(|name| {
                    format!(
                        "/{name}  {}",
                        match *name {
                            "new" if app.focus == 0 => "登记已有仓库",
                            "new" if app.focus == 1 => "新建任务",
                            "new" => "请先切到项目或任务区",
                            "delete" => "删除选中任务（确认后执行）",
                            "refresh" => "刷新状态",
                            "help" => "查看帮助",
                            "quit" => "退出控制台",
                            _ => unreachable!(),
                        }
                    )
                })
                .map(|text| Line::from(safe(&text)))
                .collect();
            list(
                frame,
                popup,
                " 命令 · 默认仍为搜索 ".into(),
                items,
                app.command_selection,
                true,
            );
        }
    }
    if app.help {
        frame.render_widget(Clear, rows[1]);
        frame.render_widget(Paragraph::new("项目 → 任务 → 任务详情（左下为只读全局监控）\n点击顶部标签 / Tab：切换 Console 与 Agent\n\n1 / 2：聚焦项目 / 任务，保留选择与筛选\nCtrl-W 后按 h/l/j/k：左侧项目 / 右侧任务 / 右下详情 / 上方任务\n↑↓ 或 j/k：选择；gg / G：列表顶部 / 底部；/：搜索当前区域，显示命令候选\nEnter：项目切到任务列表，任务进入终端工作台，详情展开最近轮次；i 展开/收起元数据；PgUp/PgDn 滚动\nn：项目区登记仓库，任务区新建任务，会话区历史下一页\ndd：任务区打开删除确认（默认取消）\nh：切换已加载与历史会话\na：历史查询包含子目录（分页扫描，不自动全量加载）\n/new /delete /refresh /help /quit：精确关键字或选择候选后 Enter 执行\n↑↓ 选择命令，Tab 补全；// 强制搜索，Esc 取消并恢复选择\nr：重新查询；Esc：取消搜索/详情或返回上一层\nq / Ctrl+C：退出导航，保留所有任务和 Agent\n终端工作台：Ctrl-W d 返回导航；Ctrl-W 后 s/v 分屏、h/j/k/l 切 Pane、z 放大；Alt-h/j/k/l 调比例\nCtrl-W f：跳转当前通知对应的任务或主控（导航与工作台通用）\nCtrl-W u：未读任务列表，j/k 选择、Enter 进入、Esc 关闭\n工作台内 Ctrl-C 发送给程序，Shift-PgUp/PgDn 浏览历史\n\n窗口已打开不代表 Codex 正在运行；空闲不代表任务完成。\n历史 notLoaded 只表示当前服务未加载，不推断其他服务状态。\n任务详情「用 Zed 打开」：Cmd/Ctrl + 点击（需配置 [zed].ssh_host 及终端链接支持）\n会话创建/更新时间为北京时间 UTC+8；Git 干净不包含 ignored 文件。\n\n任意键返回").block(block(" 帮助 ".into(),true)).wrap(Wrap{trim:false}),rows[1]);
    }
    if let Some(form) = &app.form {
        draw_form(frame, area, form);
    }
}
fn projects(frame: &mut Frame, area: Rect, app: &App) {
    let repos = app.repos();
    if repos.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.snapshot.registry_error.is_some() {
                "登记表读取失败，请检查底部错误信息；修复后按 r 重试。"
            } else if app.snapshot.registry_checked.is_none() {
                "正在读取登记表…"
            } else if !app.filters[0].is_empty() {
                "没有匹配的项目"
            } else {
                "尚未登记项目。\n按 n 登记已有仓库；\n或执行 twig add <别名> --path <路径>。"
            })
            .block(block(" 项目 ".into(), app.focus == 0))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let items = repos
        .iter()
        .map(|repo| {
            let mut spans = vec![
                Span::raw(safe(&repo.name)),
                unread(app.notifications.count(&repo.name, None)),
                Span::raw(format!("  [{}]  ", repo.tasks.len().saturating_sub(1))),
            ];
            if repo.error.is_some() {
                spans.push(Span::styled("异常", Style::default().fg(Color::Red)));
            } else {
                let stale =
                    app.snapshot.codex_error.is_some() || is_stale(app.snapshot.codex_checked);
                let mut counts = std::collections::BTreeMap::new();
                for thread in app.snapshot.threads.iter().filter(|t| {
                    app.snapshot
                        .associations
                        .get(&t.id)
                        .is_some_and(|(name, _)| name == &repo.name)
                }) {
                    let entry = counts
                        .entry(thread.label())
                        .or_insert((0, status_style(thread, stale)));
                    entry.0 += 1;
                }
                for session in claude_sessions(&app.snapshot, &repo.name, None) {
                    let entry = counts
                        .entry(format!("Claude {}", session.label()))
                        .or_insert((0, claude_style(session, &app.snapshot)));
                    entry.0 += 1;
                }
                if counts.is_empty() {
                    spans.push(Span::raw("—"));
                }
                for (index, (label, (count, style))) in counts.into_iter().enumerate() {
                    if index > 0 {
                        spans.push(Span::raw(" · "));
                    }
                    spans.push(Span::styled(safe(&format!("{label} {count}")), style));
                }
            }
            Line::from(spans)
        })
        .collect();
    list(
        frame,
        area,
        format!(" 项目 / {} ", safe(&app.filters[0])),
        items,
        repos
            .iter()
            .position(|r| Some(&r.name) == app.project.as_ref()),
        app.focus == 0,
    );
}
fn unread(count: usize) -> Span<'static> {
    if count == 0 {
        Span::raw("")
    } else {
        Span::styled(
            format!(" [未读 {count}]"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    }
}
fn tasks(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(error) = app.repo().and_then(|r| r.error.as_ref()) {
        frame.render_widget(
            Paragraph::new(safe(error))
                .block(block(" 项目异常 ".into(), app.focus == 1))
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    let tasks = app.tasks();
    let compact = area.width < 80;
    let items = tasks
        .iter()
        .map(|task| {
            let sessions = app
                .repo()
                .map(|r| data::for_task(&app.snapshot.threads, &app.snapshot, &r.name, &task.path))
                .unwrap_or_default();
            let freshness = if task.error.is_some() {
                ""
            } else if is_stale(task.checked) {
                "（状态过期）"
            } else if task.checked.is_none() {
                "（未刷新）"
            } else {
                ""
            };
            let name = safe(if task.name == "main" {
                "主工作目录"
            } else {
                &task.name
            });
            let name = Line::from(vec![
                Span::raw(name),
                unread(
                    app.notifications
                        .count(app.project.as_deref().unwrap_or_default(), Some(&task.path)),
                ),
            ]);
            let branch = safe(&task.branch);
            let git = safe(&format!("{}{freshness}", task.git));
            let window = safe(&task.window);
            let stale = app.snapshot.codex_error.is_some() || is_stale(app.snapshot.codex_checked);
            let mut counts = std::collections::BTreeMap::new();
            for thread in sessions {
                let entry = counts
                    .entry(format!("Codex {}", thread.label()))
                    .or_insert((0, status_style(thread, stale)));
                entry.0 += 1;
            }
            for session in claude_sessions(
                &app.snapshot,
                app.project.as_deref().unwrap_or_default(),
                Some(&task.path),
            ) {
                let entry = counts
                    .entry(format!("Claude {}", session.label()))
                    .or_insert((0, claude_style(session, &app.snapshot)));
                entry.0 += 1;
            }
            let mut spans = Vec::new();
            for (label, (count, style)) in counts {
                if !spans.is_empty() {
                    spans.push(Span::raw(" · "));
                }
                spans.push(Span::styled(safe(&format!("{label} {count}")), style));
            }
            if spans.is_empty() {
                spans.push(Span::raw("—"));
            }
            let codex = Line::from(spans);
            if compact {
                Row::new(vec![
                    Cell::from(Text::from(vec![name, Line::from(branch)])),
                    Cell::from(Text::from(vec![Line::from(git), Line::from(window)])),
                    Cell::from(codex),
                ])
                .height(2)
            } else {
                Row::new(vec![
                    Cell::from(name),
                    Cell::from(branch),
                    Cell::from(git),
                    Cell::from(window),
                    Cell::from(codex),
                ])
            }
        })
        .collect::<Vec<_>>();
    let (headers, widths) = if compact {
        (
            vec!["任务 / 分支", "Git / 窗口", "Agent"],
            vec![
                Constraint::Percentage(30),
                Constraint::Percentage(40),
                Constraint::Percentage(30),
            ],
        )
    } else {
        (
            vec!["任务", "分支", "Git", "窗口", "Agent"],
            vec![
                Constraint::Percentage(17),
                Constraint::Percentage(20),
                Constraint::Percentage(28),
                Constraint::Percentage(13),
                Constraint::Percentage(22),
            ],
        )
    };
    let table = Table::new(items, widths)
        .header(Row::new(headers).style(Style::default().fg(Color::DarkGray)))
        .column_spacing(1)
        .block(block(
            format!(
                " {} · 任务 / {} ",
                safe(app.project.as_deref().unwrap_or("—")),
                safe(&app.filters[1])
            ),
            app.focus == 1,
        ))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = TableState::default().with_selected(
        tasks
            .iter()
            .position(|t| Some(&t.path) == app.task.as_ref()),
    );
    frame.render_stateful_widget(table, area, &mut state);
}
fn status_style(thread: &crate::codex::Thread, stale: bool) -> Style {
    priority_style(thread.priority(), stale)
}

fn priority_style(priority: u8, stale: bool) -> Style {
    Style::default().fg(if stale {
        Color::DarkGray
    } else {
        match priority {
            0 => Color::Yellow,
            1 => Color::Red,
            2 => Color::Cyan,
            _ => Color::Gray,
        }
    })
}

fn claude_sessions<'a>(
    snapshot: &'a data::Snapshot,
    project: &str,
    path: Option<&std::path::Path>,
) -> Vec<&'a crate::claude::Session> {
    let mut sessions: Vec<_> = snapshot
        .claude
        .iter()
        .filter(|session| {
            session
                .key()
                .and_then(|key| snapshot.claude_associations.get(&key))
                .is_some_and(|(owner, task)| {
                    owner == project && path.is_none_or(|path| path == task)
                })
        })
        .collect();
    sessions.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then(a.session_id.cmp(&b.session_id))
    });
    sessions
}

fn claude_style(session: &crate::claude::Session, snapshot: &data::Snapshot) -> Style {
    priority_style(
        session.priority(),
        snapshot.claude_error.is_some() || is_stale(snapshot.claude_checked),
    )
}

enum ObservedSession<'a> {
    Codex(&'a crate::codex::Thread),
    Claude(&'a crate::claude::Session),
}
impl ObservedSession<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Codex(thread) => &thread.id,
            Self::Claude(session) => session
                .session_id
                .as_deref()
                .or(session.id.as_deref())
                .unwrap_or(""),
        }
    }
    fn title(&self) -> &str {
        match self {
            Self::Codex(t) => t.title(),
            Self::Claude(s) => s.title(),
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Codex(t) => format!("Codex {}", t.label()),
            Self::Claude(s) => format!("Claude {}", s.label()),
        }
    }
    fn priority(&self) -> u8 {
        match self {
            Self::Codex(t) => t.priority(),
            Self::Claude(s) => s.priority(),
        }
    }
    fn stale(&self, snapshot: &data::Snapshot) -> bool {
        match self {
            Self::Codex(_) => snapshot.codex_error.is_some() || is_stale(snapshot.codex_checked),
            Self::Claude(_) => snapshot.claude_error.is_some() || is_stale(snapshot.claude_checked),
        }
    }
}

// 只读取全局快照；不使用项目选择、搜索、历史分页或焦点状态。
fn monitor(frame: &mut Frame, area: Rect, app: &App) {
    let snapshot = &app.snapshot;
    let stale = snapshot.codex_error.is_some() || is_stale(snapshot.codex_checked);
    let mut grouped = std::collections::BTreeMap::<_, Vec<_>>::new();
    for thread in &snapshot.threads {
        if let Some(owner) = snapshot.associations.get(&thread.id) {
            grouped
                .entry(owner)
                .or_default()
                .push(ObservedSession::Codex(thread));
        }
    }
    for session in &snapshot.claude {
        if let Some(owner) = session
            .key()
            .and_then(|key| snapshot.claude_associations.get(&key))
        {
            grouped
                .entry(owner)
                .or_default()
                .push(ObservedSession::Claude(session));
        }
    }
    for threads in grouped.values_mut() {
        threads.sort_by(|a, b| a.priority().cmp(&b.priority()).then(a.id().cmp(b.id())));
    }
    let mut projects: Vec<_> = snapshot
        .repos
        .iter()
        .filter_map(|repo| {
            let mut tasks: Vec<_> = repo
                .tasks
                .iter()
                .filter_map(|task| {
                    let threads = grouped
                        .remove(&(repo.name.clone(), task.path.clone()))
                        .unwrap_or_default();
                    (!threads.is_empty()).then_some((task, threads))
                })
                .collect();
            tasks.sort_by(|a, b| {
                a.1[0]
                    .priority()
                    .cmp(&b.1[0].priority())
                    .then(a.0.name.cmp(&b.0.name))
            });
            (!tasks.is_empty()).then_some((repo, tasks))
        })
        .collect();
    projects.sort_by(|a, b| {
        a.1[0].1[0]
            .priority()
            .cmp(&b.1[0].1[0].priority())
            .then(a.0.name.cmp(&b.0.name))
    });
    let total: usize = projects
        .iter()
        .flat_map(|(_, tasks)| tasks)
        .map(|(_, threads)| threads.len())
        .sum();
    let panel = block(
        format!(
            " 全局 Agent · {}{} ",
            total,
            if stale || snapshot.claude_error.is_some() || is_stale(snapshot.claude_checked) {
                " · 状态过期"
            } else {
                ""
            }
        ),
        false,
    );
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    if projects.is_empty() {
        frame.render_widget(
            Paragraph::new(format!(
                "{}\n{}",
                if snapshot.codex_error.is_some() {
                    "Codex 不可用，详见底部状态"
                } else if snapshot.codex_checked.is_none() {
                    "正在读取全局会话…"
                } else {
                    "暂无已加载的关联会话"
                },
                if snapshot.claude_error.is_some() {
                    "Claude 不可用，详见任务详情"
                } else if snapshot.claude_checked.is_none() {
                    "正在读取 Claude 状态…"
                } else {
                    "Claude 暂无关联会话"
                }
            ))
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    // 标记会话行，裁剪时不留下没有会话的项目或任务标题。
    let mut rows = Vec::new();
    for (repo, tasks) in &projects {
        rows.push((
            Line::styled(
                safe(&repo.name),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            false,
        ));
        for (task_index, (task, threads)) in tasks.iter().enumerate() {
            let last_task = task_index + 1 == tasks.len();
            rows.push((
                Line::styled(
                    format!(
                        "{} {}",
                        if last_task { "└─" } else { "├─" },
                        safe(&task.name)
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
                false,
            ));
            for (index, thread) in threads.iter().enumerate() {
                let title = safe(thread.title());
                let duplicate = threads
                    .iter()
                    .filter(|other| safe(other.title()) == title)
                    .count()
                    > 1;
                let label = format!(
                    "● {}{}",
                    safe(&thread.label()),
                    if thread.stale(snapshot) {
                        "（过期）"
                    } else {
                        ""
                    }
                );
                let padding = 12_usize
                    .saturating_sub(Line::from(label.as_str()).width())
                    .max(2);
                let mut spans = vec![
                    Span::styled(
                        format!(
                            "{}{} ",
                            if last_task { "   " } else { "│  " },
                            if index + 1 == threads.len() {
                                "└─"
                            } else {
                                "├─"
                            }
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{label}{}", " ".repeat(padding)),
                        priority_style(thread.priority(), thread.stale(snapshot)),
                    ),
                    Span::raw(if title.is_empty() {
                        "未命名会话".into()
                    } else {
                        title
                    }),
                ];
                if duplicate {
                    spans.push(Span::styled(
                        format!(
                            " · {}",
                            safe(thread.id()).chars().take(8).collect::<String>()
                        ),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                rows.push((Line::from(spans), true));
            }
        }
    }
    if rows.len() > usize::from(inner.height) {
        rows.truncate(usize::from(inner.height.saturating_sub(1)));
        while rows.last().is_some_and(|(_, session)| !session) {
            rows.pop();
        }
        let visible = rows.iter().filter(|(_, session)| *session).count();
        rows.push((
            Line::styled(
                format!("另有 {} 个会话 · 优先显示需关注状态", total - visible),
                Style::default().fg(Color::DarkGray),
            ),
            false,
        ));
    }
    frame.render_widget(
        Paragraph::new(rows.into_iter().map(|(line, _)| line).collect::<Vec<_>>()),
        inner,
    );
}

fn field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), Style::default().fg(Color::DarkGray)),
        Span::raw(safe(value)),
    ])
}

pub fn zed_link(area: Rect, app: &App) -> Option<super::hyperlink::Link> {
    if area.width < 38
        || area.height < 12
        || (area.width < 100 && app.focus != 2)
        || app.help
        || app.form.is_some()
        || app.searching
        || app.snapshot.registry_error.is_some()
    {
        return None;
    }
    let inner = panels(area, app.searching)[2].inner(ratatui::layout::Margin::new(1, 1));
    let width = Line::from(super::hyperlink::LABEL).width() as u16;
    if inner.width < width || inner.height == 0 {
        return None;
    }
    Some(super::hyperlink::Link {
        area: Rect::new(inner.x, inner.y, width, 1),
        url: app.snapshot.zed.as_ref()?.link(&app.task()?.path).ok()?,
    })
}

fn zed_header(app: &App) -> Vec<Line<'static>> {
    if app.task().is_none() {
        return Vec::new();
    }
    if app.snapshot.registry_error.is_some() {
        return vec![Line::styled(
            "Zed 不可用：请先修正用户配置错误",
            Style::default().fg(Color::Red),
        )];
    }
    match &app.snapshot.zed {
        Some(config) => match config.link(&app.task().unwrap().path) {
            Ok(_) => vec![Line::styled(
                super::hyperlink::LABEL,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            )],
            Err(error) => vec![Line::styled(
                format!("Zed：{}", safe(&error.to_string())),
                Style::default().fg(Color::Red),
            )],
        },
        None => vec![
            Line::styled(
                "未配置 Zed：请设置 [zed].ssh_host",
                Style::default().fg(Color::Yellow),
            ),
            Line::from(safe(&app.registry_path.display().to_string())),
        ],
    }
}

fn sessions(frame: &mut Frame, area: Rect, app: &App) {
    let sessions = app.threads();
    let claude = app
        .task()
        .map(|task| {
            claude_sessions(
                &app.snapshot,
                app.project.as_deref().unwrap_or_default(),
                Some(&task.path),
            )
        })
        .unwrap_or_default();
    let panel = block(
        format!(
            " 任务详情 · i {}信息 · PgUp/PgDn ",
            if app.metadata { "收起" } else { "更多" }
        ),
        app.focus == 2,
    );
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    let header = zed_header(app);
    let sections = Layout::vertical([Constraint::Length(header.len() as u16), Constraint::Min(0)])
        .split(inner);
    frame.render_widget(Paragraph::new(header), sections[0]);
    let inner = sections[1];
    let selected = sessions
        .iter()
        .position(|t| Some(&t.id) == app.thread.as_ref());
    let list_height = if sessions.len() > 1 { 2 } else { 0 };
    let portions = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(list_height),
        Constraint::Min(0),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!(
            "Codex {}{} · {} 个会话 / Claude {}",
            if app.history { "历史" } else { "已加载" },
            if app.history && app.descendants {
                "+子目录"
            } else {
                ""
            },
            sessions.len(),
            claude.len()
        ))
        .style(Style::default().fg(Color::DarkGray)),
        portions[0],
    );
    if sessions.len() > 1 {
        let items: Vec<_> = sessions
            .iter()
            .map(|thread| {
                ListItem::new(format!(
                    "{}  {}",
                    thread.id.chars().take(8).collect::<String>(),
                    safe(thread.title())
                ))
            })
            .collect();
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(
            List::new(items)
                .highlight_symbol("› ")
                .highlight_style(Style::default().bg(Color::DarkGray)),
            portions[1],
            &mut state,
        );
    }
    let mut lines = Vec::new();
    if let Some(error) = &app.snapshot.claude_error {
        lines.push(Line::styled(
            format!("Claude 不可用 / 旧状态过期：{}", safe(error)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    for session in claude {
        let stale = app.snapshot.claude_error.is_some() || is_stale(app.snapshot.claude_checked);
        lines.push(Line::styled(
            format!(
                "Claude · {} · {}{}",
                safe(session.title()),
                safe(&session.label()),
                if stale { "（状态过期）" } else { "" }
            ),
            claude_style(session, &app.snapshot),
        ));
        if app.metadata {
            lines.push(field(
                "ID",
                session
                    .session_id
                    .as_deref()
                    .or(session.id.as_deref())
                    .unwrap_or("未知"),
            ));
            lines.push(field("类型", &session.kind));
            lines.push(field("目录", &session.cwd.display().to_string()));
        }
    }
    if let Some(task) = app.task()
        && let Some(error) = &task.error
    {
        lines.push(Line::styled(
            format!("! 工作区异常：{}", safe(error)),
            Style::default().fg(Color::Red),
        ));
    }
    if let Some(thread) = selected.map(|index| sessions[index]) {
        let stale = app.snapshot.codex_error.is_some() || is_stale(app.snapshot.codex_checked);
        lines.push(Line::from(vec![
            Span::styled(
                safe(thread.title()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "  ● {}{}",
                    safe(&thread.label()),
                    if stale { "（状态过期）" } else { "" }
                ),
                status_style(thread, stale),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("模型  ", Style::default().fg(Color::DarkGray)),
            Span::raw(safe(thread.model.as_deref().unwrap_or("未知"))),
            Span::styled("    推理  ", Style::default().fg(Color::DarkGray)),
            Span::raw(safe(thread.reasoning_effort.as_deref().unwrap_or("未设置"))),
        ]));
        lines.push(field(
            "更新",
            &format!("{} · 北京时间", beijing_time(thread.updated_at)),
        ));
        lines.push(Line::default());
        lines.push(Line::styled(
            "最近轮次",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if app.detail {
            match &app.snapshot.turn {
                Some((id, result)) if id == &thread.id => match result {
                    Ok(text) => lines.extend(text.lines().map(|line| Line::from(safe(line)))),
                    Err(error) => lines.push(Line::styled(
                        format!("读取失败：{}", safe(error)),
                        Style::default().fg(Color::Red),
                    )),
                },
                _ => lines.push(Line::from("正在读取…")),
            }
        } else {
            lines.push(Line::styled(
                "在详情区按 Enter 查看",
                Style::default().fg(Color::DarkGray),
            ));
        }
        if app.metadata {
            lines.push(Line::default());
            lines.push(field("ID  ", &thread.id));
            lines.push(field("来源", &thread.source.to_string()));
            lines.push(field(
                "创建",
                &format!("{} · 北京时间", beijing_time(thread.created_at)),
            ));
            if let Some(parent) = &thread.parent_thread_id {
                lines.push(field("父级", parent));
            }
        }
    } else {
        lines.push(Line::styled(
            "暂无匹配会话",
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(task) = app.task() {
        lines.push(Line::default());
        lines.push(Line::styled(
            "工作目录",
            Style::default().fg(Color::DarkGray),
        ));
        lines.push(Line::from(safe(&task.path.display().to_string())));
    }
    if app.history {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!(
                "历史已扫描 {} 条 / {} 页 · {}",
                app.snapshot.history_scanned,
                app.snapshot.history_pages,
                if app.snapshot.history_more {
                    "n 下一页"
                } else {
                    "已到末页"
                }
            ),
            Style::default().fg(Color::DarkGray),
        ));
        if let Some(error) = &app.snapshot.history_error {
            lines.push(Line::styled(
                format!("历史读取失败：{}", safe(error)),
                Style::default().fg(Color::Red),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.scroll, 0))
            .wrap(Wrap { trim: false }),
        portions[2],
    );
}

fn form_area(area: Rect, form: &super::form::Form) -> Rect {
    let (max_width, max_height) = if form.deletion.is_some() {
        (78, 22)
    } else {
        (72, 20)
    };
    let width = area.width.saturating_sub(4).min(max_width);
    let height = area.height.saturating_sub(2).min(max_height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn draw_form(frame: &mut Frame, area: Rect, form: &super::form::Form) {
    let area = form_area(area, form);
    if let Some(deletion) = &form.deletion {
        let border = block(" 删除任务 ".into(), true).title_alignment(Alignment::Center);
        let inner = border.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(border, area);
        let compact = inner.height < 14;
        let rows = Layout::vertical([
            Constraint::Length(if compact { 2 } else { 4 }),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if compact { 2 } else { 3 }),
        ])
        .split(inner);
        let identity = format!(
            "项目：{} · 任务：{}\n分支：{}\nWorktree：{}",
            safe(form.project.as_deref().unwrap_or_default()),
            safe(&form.fields[0]),
            safe(&deletion.branch),
            safe(&deletion.path.to_string_lossy())
        );
        frame.render_widget(
            Paragraph::new(identity)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false }),
            rows[0],
        );
        for (i, (label, checked)) in [
            ("删除本地分支", deletion.delete_branch),
            ("同时删除被忽略文件", deletion.delete_ignored),
        ]
        .iter()
        .enumerate()
        {
            let selected = deletion.option == i;
            let style = if selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            frame.render_widget(
                Paragraph::new(format!(
                    "{} [{}] {label}",
                    if selected { "›" } else { " " },
                    if *checked { "x" } else { " " }
                ))
                .alignment(Alignment::Center)
                .style(style),
                Rect {
                    y: rows[1].y + i as u16,
                    height: 1,
                    ..rows[1]
                },
            );
        }
        let mut details = Vec::new();
        if let Some(error) = &form.error {
            details.push(format!(
                "删除失败：{}\n可能已完成部分清理；请检查后重试。",
                safe(error)
            ));
        }
        details.push(format!(
            "Worktree：{}",
            safe(&deletion.path.to_string_lossy())
        ));
        details.push(format!(
            "删除窗口、Worktree 和任务记录{}。",
            if !deletion.delete_branch {
                "，保留分支"
            } else {
                "及本地分支"
            }
        ));
        if !deletion.delete_branch {
            details.push("保留分支时跳过合并/推送检查。".into());
        } else {
            details.push("不检查合并和推送状态，未合并或未推送的本地提交也会随分支删除。".into());
        }
        if !deletion.ignored.is_empty() {
            details.push(format!(
                "被忽略文件/目录（目录内全部内容）：\n{}",
                deletion
                    .ignored
                    .iter()
                    .map(|s| safe(s))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        frame.render_widget(
            Paragraph::new(details.join("\n"))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false })
                .scroll((form.scroll, 0)),
            rows[2],
        );
        let buttons = if form.busy() {
            form.progress.clone()
        } else {
            format!(
                "{}[ 取消 ]    {}[ 确认删除 ]",
                if deletion.confirm { "  " } else { "› " },
                if deletion.confirm { "› " } else { "  " }
            )
        };
        frame.render_widget(
            Paragraph::new(buttons)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Cyan)),
            rows[3],
        );
        let hint = if compact {
            "↑↓ 选项 · 空格勾选 · Tab 按钮\nEnter 执行 · Esc 取消 · PgUp/Dn 滚动"
        } else {
            "↑↓ / j k 选择选项 · 空格切换\nTab / ←→ 切换按钮 · Enter 执行 · Esc 取消\nPgUp/PgDn 滚动清单及错误"
        };
        frame.render_widget(
            Paragraph::new(hint)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Gray)),
            rows[4],
        );
        return;
    }
    if let Some(existing) = &form.existing {
        let border = block(" 使用已有分支 ".into(), true);
        let inner = border.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(border, area);
        let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
        let source_hint = if existing.branches.len() > 1 {
            format!(
                "↑↓ 选择来源（{}/{}） · PgUp/PgDn 滚动",
                existing.selected + 1,
                existing.branches.len()
            )
        } else {
            "PgUp/PgDn 滚动说明".into()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "分支 {} 已存在，是否使用它创建任务？\n保留已有提交，忽略起点分支；远端来源建立跟踪。\n{}",
                safe(existing.branches[existing.selected].label()),
                source_hint,
            )).wrap(Wrap { trim: false }).scroll((form.scroll, 0)),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}取消    {}使用分支\nTab / ←→ 切换 · Enter 确认\nEsc 返回表单",
                if existing.confirmed { "  " } else { "› " },
                if existing.confirmed { "› " } else { "  " },
            )),
            rows[1],
        );
        return;
    }
    if let Some(project) = &form.project {
        draw_task_form(frame, area, form, project);
        return;
    }
    let border = block(" 登记已有仓库 ".into(), true);
    let inner = border.inner(area).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, area);
    frame.render_widget(border, area);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new("登记已有 Git 仓库").style(Style::default().add_modifier(Modifier::BOLD)),
        sections[0],
    );
    let labels = ["仓库路径（~ / 相对路径）", "项目别名"];
    for (i, label) in labels.iter().enumerate() {
        let style = if form.field == i {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        draw_form_field(frame, sections[i + 1], form, i, label, style);
    }
    frame.render_widget(
        Paragraph::new(if form.busy() {
            "正在执行，请等待…\n执行期间不强制取消，避免中断写操作。"
        } else if form.completing_path() {
            "↑↓ 选目录 · Tab 补全/切字段\nEnter 下一步 · Esc 取消"
        } else {
            "Enter 登记 · Esc 取消\nShift+Tab 返回 · PgUp/Dn 滚动"
        }),
        sections[4],
    );
    if form.completing_path() && form.error.is_none() && !form.directories.is_empty() {
        let items = form
            .directories
            .iter()
            .map(|path| {
                format!(
                    "{}/",
                    path.trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or(path)
                )
            })
            .map(|text| Line::from(safe(&text)))
            .collect();
        if sections[3].height < 3 {
            let mut state = ListState::default().with_selected(Some(form.directory_selection));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(Style::default().bg(Color::DarkGray))
                    .highlight_symbol("› "),
                sections[3],
                &mut state,
            );
        } else {
            list(
                frame,
                sections[3],
                " 本地目录 · Tab 补全 ".into(),
                items,
                Some(form.directory_selection),
                true,
            );
        }
        return;
    }
    let message = match &form.error {
        Some(error) if form.operation_started => format!(
            "操作失败：{}\n已创建的资源会保留；修正输入或处理错误后可重试。",
            safe(error)
        ),
        Some(error) => format!("请检查输入：{}", safe(error)),
        None if form.completing_path() && form.directory_error.is_some() => format!(
            "路径补全失败：{}",
            safe(form.directory_error.as_deref().unwrap())
        ),
        None => {
            "登记已有 Git 仓库；未初始化时自动初始化，不克隆、不创建空仓库。已有登记复用原别名。"
                .into()
        }
    };
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .scroll((form.scroll, 0))
            .style(Style::default().fg(if form.error.is_some() {
                Color::Red
            } else {
                Color::Gray
            })),
        sections[3],
    );
}

fn draw_form_field(
    frame: &mut Frame,
    area: Rect,
    form: &super::form::Form,
    index: usize,
    label: &str,
    style: Style,
) {
    let focused = form.field == index;
    frame.render_widget(
        Paragraph::new(format!("{} {label}", if focused { "›" } else { " " })).style(style),
        Rect { height: 1, ..area },
    );
    if area.height > 1 {
        let value = Line::from(format!(
            "{}{}",
            safe(&form.fields[index]),
            if focused && !form.busy() { "▏" } else { "" }
        ));
        let offset = value
            .width()
            .saturating_sub(area.width as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(
            Paragraph::new(value)
                .style(style.bg(Color::DarkGray))
                .scroll((0, offset)),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
    }
}

fn draw_task_form(frame: &mut Frame, area: Rect, form: &super::form::Form, project: &str) {
    let border = block(" 新建任务 ".into(), true);
    let inner = border.inner(area).inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(Clear, area);
    frame.render_widget(border, area);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(if inner.height >= 14 { 1 } else { 0 }),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(format!("项目：{}", safe(project)))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        sections[0],
    );
    let base_label = if form
        .branches
        .iter()
        .any(|branch| branch.name == form.fields[0])
    {
        "起点分支（复用时忽略）"
    } else if !form.fetch_base {
        "起点分支（本地）"
    } else {
        "起点分支（读取其 upstream）"
    };
    for (i, (label, area)) in [("任务名", sections[2]), (base_label, sections[4])]
        .into_iter()
        .enumerate()
    {
        let style = Style::default().fg(if form.field == i {
            Color::Cyan
        } else {
            Color::Gray
        });
        draw_form_field(frame, area, form, i, label, style);
    }
    if form.completing_branch() && form.error.is_none() && form.branch_error.is_none() {
        let items: Vec<_> = form
            .matching_branches()
            .map(|branch| ListItem::new(safe(branch.label())))
            .collect();
        if items.is_empty() {
            frame.render_widget(Paragraph::new("无匹配分支，将新建分支"), sections[3]);
        } else {
            let mut widget = List::new(items)
                .highlight_style(Style::default().bg(Color::DarkGray))
                .highlight_symbol("› ");
            if sections[3].height >= 3 {
                widget = widget.block(block(" 已有分支 · 选作任务 ".into(), true));
            }
            let mut state = ListState::default().with_selected(form.branch_selection);
            frame.render_stateful_widget(widget, sections[3], &mut state);
        }
    } else {
        let message = match &form.error {
            Some(error) if form.operation_started => {
                format!(
                    "操作失败：{}。若已有资源和记录则保留；已登记任务请使用 open，尚未创建时可修复后重试。",
                    safe(error)
                )
            }
            Some(error) => format!("请检查输入：{}", safe(error)),
            None if form.busy() => form.progress.clone(),
            None if form.completing_branch() && form.branch_error.is_some() => format!(
                "读取分支失败：{}",
                safe(form.branch_error.as_deref().unwrap())
            ),
            None => "创建成功后直接进入工作台，Ctrl-W d 返回导航。".into(),
        };
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: false })
                .scroll((form.scroll, 0))
                .style(Style::default().fg(if form.error.is_some() {
                    Color::Red
                } else {
                    Color::Gray
                })),
            sections[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "[{}] 更新远端 base（复用已有分支时忽略）",
            if form.fetch_base { "x" } else { " " }
        ))
        .style(Style::default().fg(if form.field == 2 {
            Color::Cyan
        } else {
            Color::Gray
        })),
        sections[5],
    );
    frame.render_widget(
        Paragraph::new(if form.busy() {
            "执行期间请等待，避免中断写操作。"
        } else if form.completing_branch() {
            if form.branch_selection.is_some() {
                "↑↓ 选择 · Tab 填入\nEnter 选中并确认 · Esc 取消"
            } else {
                "↑↓ 选择 · Tab 填入\nEnter 按输入创建 · Esc 取消"
            }
        } else {
            "Tab 切字段 · 空格勾选 · Enter 创建\nEsc 取消 · PgUp/PgDn 滚动"
        })
        .wrap(Wrap { trim: false }),
        sections[6],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
    use serde_json::json;

    #[test]
    fn zed_entry_uses_selected_task_and_respects_visibility() {
        let mut app = fixture();
        app.registry_path = "/custom/twig.toml".into();
        let area = Rect::new(0, 0, 140, 40);
        assert!(zed_link(area, &app).is_none());
        let output = text(&render(&app, 140, 40, |frame, _, app| draw(frame, app)));
        assert!(output.contains("[zed].ssh_host"));
        assert!(output.contains("/custom/twig.toml"));
        app.snapshot.zed = Some(crate::zed::Config {
            ssh_host: "devbox".into(),
        });
        let link = zed_link(area, &app).unwrap();
        assert_eq!(
            link.url,
            format!("zed://ssh/devbox{}", app.task().unwrap().path.display())
        );
        let buffer = render(&app, 140, 40, |frame, _, app| draw(frame, app));
        let mut output = Vec::new();
        super::super::hyperlink::Painter::default()
            .draw(&mut output, &buffer, Some(&link))
            .unwrap();
        assert!(String::from_utf8_lossy(&output).contains(&link.url));
        app.focus = 1;
        assert!(zed_link(Rect::new(0, 0, 65, 24), &app).is_none());
        app.focus = 2;
        assert!(zed_link(Rect::new(0, 0, 65, 24), &app).is_some());
        app.help = true;
        assert!(zed_link(area, &app).is_none());
        app.help = false;
        app.snapshot.registry_error = Some("bad config".into());
        assert!(zed_link(area, &app).is_none());
    }

    #[test]
    fn form_selection_uses_only_the_visible_dialog_in_all_four_forms() {
        use super::super::form::{Deletion, ExistingBranch, Form};
        let mut app = fixture();
        let mut existing = Form::task("alpha".into(), "main".into());
        existing.existing = Some(ExistingBranch {
            branches: vec![crate::config::BranchStart {
                reference: "refs/heads/existing".into(),
                commit: "abc".into(),
            }],
            common: "/tmp/repo".into(),
            selected: 0,
            confirmed: false,
        });
        let mut deletion = Form::task("alpha".into(), "main".into());
        deletion.deletion = Some(Deletion {
            branch: "task".into(),
            path: "/tmp/task".into(),
            expected: "abc".into(),
            delete_branch: true,
            delete_ignored: true,
            ignored: vec![],
            confirm: false,
            option: 0,
        });
        for form in [
            Form::project(),
            Form::task("alpha".into(), "main".into()),
            existing,
            deletion,
        ] {
            app.form = Some(form);
            for (width, height) in [(38, 12), (80, 24), (140, 36)] {
                let buffer = render(&app, width, height, |frame, _, app| super::draw(frame, app));
                let regions = selection_regions(buffer.area, &app);
                let inner = form_area(buffer.area, app.form.as_ref().unwrap())
                    .inner(ratatui::layout::Margin::new(1, 1));
                assert_eq!(regions, vec![inner]);
                assert!(!inner.contains((0, 0).into()));
                let mut selection =
                    super::super::selection::Selection::new(&buffer, inner, inner.x, inner.y);
                selection.update(width - 1, height - 1);
                let copied = selection.text();
                assert!(!copied.is_empty());
                for background in ["Console", "全局 Agent", "│", "┌", "┘"] {
                    assert!(!copied.contains(background), "{copied}");
                }
            }
        }
    }

    #[test]
    fn task_dialog_is_centered_and_preserves_background_and_selection() {
        let mut app = fixture();
        let selected = (app.project.clone(), app.task.clone());
        app.snapshot.repos.reverse();
        app.reconcile();
        assert_eq!(selected, (app.project.clone(), app.task.clone()));
        app.form = Some(super::super::form::Form::task(
            "alpha".into(),
            "main".into(),
        ));
        for (width, height) in [(38, 12), (65, 24), (100, 30), (140, 40)] {
            let buffer = render(&app, width, height, |frame, _, app| super::draw(frame, app));
            let output = text(&buffer);
            for expected in [
                "新建任务",
                "项目：alpha",
                "任务名",
                "起点分支",
                "Enter 按输入创建",
                "Esc 取消",
            ] {
                assert!(
                    output.contains(expected),
                    "{width}×{height}: {expected}\n{output}"
                );
            }
            let popup_width = (width - 4).min(72);
            let popup_height = (height - 2).min(20);
            let x = (width - popup_width) / 2;
            let y = (height - popup_height) / 2;
            assert_eq!(buffer[(x, y)].symbol(), "┌");
            assert_eq!(
                buffer[(x + popup_width - 1, y + popup_height - 1)].symbol(),
                "┘"
            );
            // 小屏弹窗可遮住标签文字，顶部圆角边框仍保留导航背景。
            assert!(output.lines().next().unwrap().contains("╭"));
            if y >= 3 {
                assert!(output.contains("Console") && output.contains("Agent"));
            }
        }
    }

    #[test]
    fn project_dialog_keeps_candidates_errors_and_actions_visible() {
        let mut app = fixture();
        app.form = Some(super::super::form::Form::project());
        for (width, height) in [(38, 12), (65, 24), (100, 30), (140, 40)] {
            let form = app.form.as_mut().unwrap();
            form.error = None;
            form.directories = vec!["/projects/alpha/".into(), "/projects/beta/".into()];
            form.directory_selection = 1;
            let buffer = render(&app, width, height, |frame, _, app| super::draw(frame, app));
            let output = text(&buffer);
            for expected in [
                "登记已有仓库",
                "仓库路径",
                "项目别名",
                "beta/",
                "Enter 下一步",
                "Esc 取消",
            ] {
                assert!(
                    output.contains(expected),
                    "{width}×{height}: {expected}\n{output}"
                );
            }
            let x = (width - (width - 4).min(72)) / 2;
            let y = (height - (height - 2).min(20)) / 2;
            assert_eq!(buffer[(x, y)].symbol(), "┌");
            app.form.as_mut().unwrap().error = Some("目录不存在".into());
            let output = text(&render(&app, width, height, |frame, _, app| {
                super::draw(frame, app)
            }));
            assert!(output.contains("目录不存在"));
            assert!(output.contains("Esc 取消"));
            assert!(!output.contains("beta/"));
        }
    }

    #[test]
    fn branch_picker_keeps_selection_and_actions_visible_on_small_screens() {
        let mut app = fixture();
        let mut form = super::super::form::Form::task("alpha".into(), "".into());
        form.field = 0;
        form.branches = (0..30)
            .map(|i| {
                let name = format!("feature/{i:02}");
                super::super::form::BranchCandidate {
                    reference: format!("refs/heads/{name}"),
                    name,
                }
            })
            .collect();
        form.branch_selection = Some(29);
        app.form = Some(form);
        for (width, height) in [(38, 12), (65, 24), (100, 30)] {
            let output = text(&render(&app, width, height, |frame, _, app| {
                super::draw(frame, app)
            }));
            for expected in ["feature/29", "Tab 填入", "Enter 选中并确认", "Esc 取消"] {
                assert!(
                    output.contains(expected),
                    "{width}×{height}: {expected}\n{output}"
                );
            }
        }
        app.form.as_mut().unwrap().fields[0] = "absent".into();
        assert!(
            text(&render(&app, 65, 24, |frame, _, app| super::draw(
                frame, app
            )))
            .contains("无匹配分支，将新建分支")
        );
        app.form.as_mut().unwrap().branch_error = Some("读取失败".into());
        assert!(
            text(&render(&app, 65, 24, |frame, _, app| super::draw(
                frame, app
            )))
            .contains("读取分支失败")
        );
    }

    fn fixture() -> App {
        let mut app = App::new("/alpha".into());
        app.snapshot.codex_error = None;
        app.snapshot.codex_checked = Some(Instant::now());
        for (project, status) in [("alpha", "idle"), ("beta", "active")] {
            let path = std::path::PathBuf::from(format!("/{project}"));
            app.snapshot.repos.push(data::Repo {
                name: project.into(),
                path: path.clone(),
                error: None,
                context: None,
                tasks: vec![data::Task {
                    name: "main".into(),
                    path: path.clone(),
                    branch: "main".into(),
                    git: "干净".into(),
                    window: "已打开".into(),
                    error: None,
                    checked: None,
                }],
            });
            app.snapshot.threads.push(serde_json::from_value(json!({
                "id": format!("{project}-full-id"), "cwd": path,
                "status": {"type": status, "activeFlags": []},
                "name": format!("{project} 会话"), "model": "test-model", "reasoningEffort": "high",
                "source": "cli", "updatedAt": 1, "createdAt": 1,
            })).unwrap());
            app.snapshot
                .associations
                .insert(format!("{project}-full-id"), (project.into(), path));
        }
        app.reconcile();
        app
    }

    fn render(app: &App, width: u16, height: u16, draw: fn(&mut Frame, Rect, &App)) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw(frame, frame.area(), app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buffer: &Buffer) -> String {
        let mut text = String::new();
        for y in 0..buffer.area.height {
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                text.push_str(symbol);
                x += Span::raw(symbol).width().max(1) as u16;
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn global_monitor_is_independent_of_navigation_search_and_history() {
        let mut app = fixture();
        let before = render(&app, 55, 12, monitor);
        let output = text(&before);
        assert!(output.find("beta").unwrap() < output.find("alpha").unwrap());
        for focus in 0..3 {
            app.focus = focus;
            app.project = Some("beta".into());
            app.task = Some("/beta".into());
            app.filters = ["missing".into(), "missing".into(), "missing".into()];
            app.history = true;
            app.metadata = true;
            app.scroll = 12;
            app.reconcile();
            assert_eq!(before, render(&app, 55, 12, monitor));
        }
        app.snapshot.threads[1].status = json!({"type":"idle"});
        assert_ne!(before, render(&app, 55, 12, monitor));
    }

    #[test]
    fn claude_monitor_and_details_keep_multiple_sessions_and_provider_freshness() {
        let mut app = fixture();
        app.snapshot.claude_checked = Some(Instant::now());
        for (id, status) in [("claude-one", "busy"), ("claude-two", "waiting")] {
            let session: crate::claude::Session = serde_json::from_value(json!({
                "cwd":"/alpha", "kind":"interactive", "startedAt":1,
                "sessionId":id, "name":id, "status":status,
                "waitingFor":"permission prompt"
            }))
            .unwrap();
            app.snapshot
                .claude_associations
                .insert(session.key().unwrap(), ("alpha".into(), "/alpha".into()));
            app.snapshot.claude.push(session);
        }
        let before = render(&app, 100, 20, monitor);
        let output = text(&before);
        assert!(output.contains("claude-one") && output.contains("claude-two"));
        assert!(output.contains("Claude 等待处理"));
        app.history = true;
        app.filters = ["absent".into(), "absent".into(), "absent".into()];
        assert_eq!(before, render(&app, 100, 20, monitor));
        app.filters = Default::default();
        app.history = false;
        app.project = Some("alpha".into());
        app.task = Some("/alpha".into());
        let detail = text(&render(&app, 100, 30, sessions));
        assert!(detail.contains("claude-one") && detail.contains("claude-two"));
        app.snapshot.claude_error = Some("mock unavailable".into());
        let output = text(&render(&app, 100, 20, monitor));
        assert!(
            output
                .lines()
                .any(|line| line.contains("Claude") && line.contains("过期"))
        );
        assert!(
            !output
                .lines()
                .any(|line| line.contains("● Codex") && line.contains("过期"))
        );
        assert!(text(&render(&app, 100, 30, sessions)).contains("mock unavailable"));
        assert!(text(&render(&app, 55, 5, monitor)).contains("另有"));
    }

    #[test]
    fn monitor_marks_overflow_staleness_and_unavailable_service() {
        let mut app = fixture();
        let output = text(&render(&app, 55, 6, monitor));
        assert!(output.contains("beta 会话"));
        assert!(output.contains("另有 1 个会话"));
        assert!(!output.contains("alpha 会话"));
        app.snapshot.codex_error = Some("offline".into());
        assert!(text(&render(&app, 55, 8, monitor)).contains("状态过期"));
        app.snapshot.threads.clear();
        assert!(text(&render(&app, 55, 8, monitor)).contains("Codex 不可用"));
    }

    #[test]
    fn monitor_groups_sessions_and_prioritizes_projects_and_tasks() {
        let mut app = fixture();
        let mut duplicate = app.snapshot.threads[0].clone();
        duplicate.id = "copyabcd-full-id".into();
        app.snapshot
            .associations
            .insert(duplicate.id.clone(), ("alpha".into(), "/alpha".into()));
        app.snapshot.threads.push(duplicate);
        let mut task = app.snapshot.repos[0].tasks[0].clone();
        task.name = "urgent".into();
        task.path = "/alpha/urgent".into();
        let mut thread = app.snapshot.threads[0].clone();
        thread.id = "urgent-session".into();
        thread.name = Some("需要确认".into());
        thread.cwd = task.path.clone();
        thread.status = json!({"type":"active", "activeFlags":["waitingOnUserInput"]});
        app.snapshot
            .associations
            .insert(thread.id.clone(), ("alpha".into(), task.path.clone()));
        app.snapshot.threads.push(thread);
        app.snapshot.repos[0].tasks.push(task);
        let output = text(&render(&app, 70, 18, monitor));
        assert!(output.find("alpha").unwrap() < output.find("beta").unwrap());
        assert!(output.find("├─ urgent").unwrap() < output.find("└─ main").unwrap());
        assert_eq!(output.matches("alpha 会话").count(), 2);
        assert!(output.contains("alpha-fu"));
        assert!(output.contains("copyabcd"));
        assert!(
            output
                .lines()
                .any(|line| line.contains("等待输入") && line.contains("需要确认"))
        );

        let app = fixture();
        let short = text(&render(&app, 55, 5, monitor));
        assert!(short.contains("另有 2 个会话"));
        assert!(!short.contains("beta"));
        let clipped = text(&render(&app, 55, 7, monitor));
        assert!(clipped.contains("beta 会话"));
        assert!(!clipped.contains("alpha"));
        assert!(clipped.contains("另有 1 个会话"));
    }

    #[test]
    fn footer_selection_freezes_only_status_and_is_disabled_in_overlays() {
        let mut app = fixture();
        app.message = Some("Codex 状态错误：连接中断".into());
        let mut buffer = render(&app, 80, 24, |frame, _, app| super::draw(frame, app));
        let area = *selection_regions(buffer.area, &app).last().unwrap();
        let mut selected = super::super::selection::Selection::new(&buffer, area, 2, 23);
        selected.select_click(3);
        assert_eq!(selected.text(), "Codex 状态错误：连接中断");
        app.message = Some("Codex 已连接".into());
        buffer = render(&app, 80, 24, |frame, _, app| super::draw(frame, app));
        selected.draw(&mut buffer);
        assert!(text(&buffer).contains("Codex 状态错误：连接中断"));
        assert!(
            text(&render(&app, 80, 24, |frame, _, app| super::draw(
                frame, app
            )))
            .contains("Codex 已连接")
        );
        app.help = true;
        assert!(selection_regions(buffer.area, &app).is_empty());
        app.help = false;
        app.searching = true;
        assert!(selection_regions(buffer.area, &app).is_empty());
    }

    #[test]
    fn monitor_selection_freezes_only_its_region_until_cleared() {
        for width in [80, 140] {
            let mut app = fixture();
            let buffer = render(&app, width, 30, |frame, _, app| super::draw(frame, app));
            let area = panels(buffer.area, false)[3].inner(ratatui::layout::Margin::new(1, 1));
            assert!(selection_regions(buffer.area, &app).contains(&area));
            let mut selected =
                super::super::selection::Selection::new(&buffer, area, area.x, area.y);
            selected.update(width - 1, 29);
            let copied = selected.text();
            assert!(!copied.is_empty());
            assert!(!copied.contains('│'));
            app.message = Some("后台继续刷新".into());
            app.snapshot.codex_error = Some("模拟断连".into());
            app.snapshot.threads.clear();
            let mut updated = render(&app, width, 30, |frame, _, app| super::draw(frame, app));
            assert!(text(&updated).contains("Codex 不可用"));
            selected.draw(&mut updated);
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    assert_eq!(updated[(x, y)].symbol(), buffer[(x, y)].symbol());
                }
            }
            assert!(text(&updated).contains("后台继续刷新"));
            assert!(
                text(&render(&app, width, 30, |frame, _, app| super::draw(
                    frame, app
                )))
                .contains("Codex 不可用")
            );
        }
    }

    #[test]
    fn layout_keeps_monitor_visible_and_details_hide_metadata() {
        let mut app = fixture();
        for (width, height) in [(38, 12), (65, 22), (100, 30), (140, 40)] {
            for focus in 0..3 {
                app.focus = focus;
                let area = Rect::new(0, 0, width, height);
                let regions = selection_regions(area, &app);
                let monitor_area =
                    panels(area, app.searching)[3].inner(ratatui::layout::Margin::new(1, 1));
                let monitor_visible = monitor_area.width > 0 && monitor_area.height > 0;
                assert_eq!(
                    regions.len(),
                    (if width >= 100 { 4 } else { 2 }) + usize::from(monitor_visible)
                );
                assert_eq!(regions.last(), Some(&Rect::new(0, height - 1, width, 1)));
                assert_eq!(regions.contains(&monitor_area), monitor_visible);
                let output = text(&render(&app, width, height, |frame, _, app| {
                    super::draw(frame, app)
                }));
                assert!(
                    output.contains("全局 Agent"),
                    "{width}x{height}, focus {focus}"
                );
            }
        }
        let output = text(&render(&app, 70, 25, sessions));
        assert!(output.contains("test-model"));
        assert!(!output.contains("alpha-full-id"));
        app.metadata = true;
        assert!(text(&render(&app, 70, 25, sessions)).contains("alpha-full-id"));
    }
}
