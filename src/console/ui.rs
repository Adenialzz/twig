use super::{App, data};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
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
    items: Vec<String>,
    selected: Option<usize>,
    focused: bool,
) {
    let widget = List::new(
        items
            .into_iter()
            .map(|s| ListItem::new(safe(&s)))
            .collect::<Vec<_>>(),
    )
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
fn panels(area: Rect) -> [Rect; 3] {
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .split(area);
    if area.width < 100 {
        return [rows[1]; 3];
    }
    let columns =
        Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(72)]).split(rows[1]);
    let right = Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(columns[1]);
    [columns[0], right[0], right[1]]
}

pub fn selection_regions(area: Rect, app: &App) -> Vec<Rect> {
    if area.width < 38 || area.height < 12 || app.form.is_some() || app.help || app.searching {
        return Vec::new();
    }
    let areas = panels(area);
    let mut regions = Vec::new();
    for (index, panel) in areas.into_iter().enumerate() {
        if area.width < 100 && index != app.focus {
            continue;
        }
        if index == 2 {
            regions.extend(
                Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(panel)
                    .iter()
                    .copied(),
            );
        } else {
            regions.push(panel);
        }
    }
    regions
        .into_iter()
        .map(|area| area.inner(ratatui::layout::Margin::new(1, 1)))
        .filter(|area| area.width > 0 && area.height > 0)
        .collect()
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 38 || area.height < 12 {
        frame.render_widget(Paragraph::new("终端过小，请扩大到至少 38×12；q 退出"), area);
        return;
    }
    if let Some(form) = &app.form
        && form.deletion.is_none()
    {
        draw_form(frame, area, form);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(3),
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
        rows[0],
    );
    if area.width >= 100 {
        let areas = panels(area);
        projects(frame, areas[0], app);
        tasks(frame, areas[1], app);
        sessions(frame, areas[2], app);
    } else {
        match app.focus {
            0 => projects(frame, rows[1], app),
            1 => tasks(frame, rows[1], app),
            _ => sessions(frame, rows[1], app),
        }
    }
    let message = app
        .message
        .as_deref()
        .or(app.snapshot.registry_error.as_deref())
        .or(app.snapshot.codex_error.as_deref());
    let connection = message.map(safe).unwrap_or_else(|| {
        match app.snapshot.codex_checked {
            None => "正在读取 Codex 状态…",
            Some(at) if is_stale(Some(at)) => "Codex 状态过期 · 超过 30 秒未更新 · r 刷新",
            Some(_) => "Codex 已连接 · 仅当前服务可见会话",
        }
        .into()
    });
    let hint = if app.pane_prefix {
        "Ctrl-W：h 左侧项目 · l 右侧任务 · j 下方会话 · k 上方任务 · Esc 取消".into()
    } else if app.top_prefix {
        "g：再按 g 跳到列表顶部 · Esc 取消".into()
    } else if app.delete_prefix {
        "d：再按 d 打开删除确认 · Esc 取消".into()
    } else if app.searching {
        format!(
            "搜索：/{}  [↑↓ 命令 · Tab 补全 · Enter 确认 · Esc 取消]",
            safe(&app.search_input)
        )
    } else {
        "Tab / Ctrl-W hjkl 切区  ↑↓ 选择  Enter 进入/详情  / 搜索  n 新建/下一页  h 历史  r 刷新  ? 帮助  q 退出"
            .into()
    };
    frame.render_widget(
        Paragraph::new(vec![Line::from(connection), Line::from(hint)]).wrap(Wrap { trim: false }),
        rows[2],
    );
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
        frame.render_widget(Paragraph::new("项目 → 任务 → Codex 会话\n\nTab / Shift+Tab：切换区域；窄终端按区域切页\nCtrl-W 后按 h/l/j/k：左侧项目 / 右侧任务 / 下方会话 / 上方任务\n↑↓ 或 j/k：选择；gg / G：列表顶部 / 底部；/：搜索当前区域，显示命令候选\nEnter：项目切到任务列表，任务进入终端工作台，会话展开最近轮次\nn：项目区登记仓库，任务区新建任务，会话区历史下一页\ndd：任务区打开删除确认（默认取消）\nh：切换已加载与历史会话\na：历史查询包含子目录（分页扫描，不自动全量加载）\n/new /delete /refresh /help /quit：精确关键字或选择候选后 Enter 执行\n↑↓ 选择命令，Tab 补全；// 强制搜索，Esc 取消并恢复选择\nr：重新查询；Esc：取消搜索/详情或返回上一层\nq / Ctrl+C：退出导航，保留所有任务和 Agent\n终端工作台：Ctrl-W d 返回导航；Ctrl-W 后 s/v 分屏、h/j/k/l 切 Pane、z 放大；Alt-h/j/k/l 调比例\n工作台内 Ctrl-C 发送给程序，Shift-PgUp/PgDn 浏览历史\n\n窗口已打开不代表 Codex 正在运行；空闲不代表任务完成。\n历史 notLoaded 只表示当前服务未加载，不推断其他服务状态。\n会话创建/更新时间为北京时间 UTC+8；Git 干净不包含 ignored 文件。\n\n任意键返回").block(block(" 帮助 ".into(),true)).wrap(Wrap{trim:false}),rows[1]);
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
            let summary = data::summary(app.snapshot.threads.iter().filter(|t| {
                app.snapshot
                    .associations
                    .get(&t.id)
                    .is_some_and(|(name, _)| name == &repo.name)
            }));
            format!(
                "{}{}  [{}]  {}",
                repo.name,
                unread(app.notifications.count(&repo.name, None)),
                repo.tasks.len().saturating_sub(1),
                if repo.error.is_some() {
                    "异常".into()
                } else {
                    summary
                }
            )
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
fn unread(count: usize) -> String {
    if count == 0 {
        String::new()
    } else {
        format!(" [未读 {count}]")
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
            let name = format!(
                "{name}{}",
                unread(
                    app.notifications
                        .count(app.project.as_deref().unwrap_or_default(), Some(&task.path))
                )
            );
            let branch = safe(&task.branch);
            let git = safe(&format!("{}{freshness}", task.git));
            let window = safe(&task.window);
            let codex = safe(&data::summary(sessions.into_iter()));
            if compact {
                Row::new(vec![
                    Cell::from(Text::from(vec![Line::from(name), Line::from(branch)])),
                    Cell::from(Text::from(vec![Line::from(git), Line::from(window)])),
                    Cell::from(codex),
                ])
                .height(2)
            } else {
                Row::new(vec![name, branch, git, window, codex])
            }
        })
        .collect::<Vec<_>>();
    let (headers, widths) = if compact {
        (
            vec!["任务 / 分支", "Git / 窗口", "Codex"],
            vec![
                Constraint::Percentage(30),
                Constraint::Percentage(40),
                Constraint::Percentage(30),
            ],
        )
    } else {
        (
            vec!["任务", "分支", "Git", "窗口", "Codex"],
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
fn sessions(frame: &mut Frame, area: Rect, app: &App) {
    let sessions = app.threads();
    let title = format!(
        " Codex {}{} · {} ",
        if app.history { "历史" } else { "已加载" },
        if app.history && app.descendants {
            "+子目录"
        } else {
            ""
        },
        if app.snapshot.codex_error.is_some() || is_stale(app.snapshot.codex_checked) {
            "状态过期"
        } else {
            "当前服务"
        }
    );
    let portions =
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let items = sessions
        .iter()
        .map(|t| {
            format!(
                "{}  {}  {}",
                t.label(),
                t.id.chars().take(8).collect::<String>(),
                t.title()
            )
        })
        .collect();
    list(
        frame,
        portions[0],
        title,
        items,
        sessions
            .iter()
            .position(|t| Some(&t.id) == app.thread.as_ref()),
        app.focus == 2,
    );
    let mut lines = Vec::new();
    if let Some(task) = app.task() {
        lines.push(format!("目录：{}", task.path.display()));
        if let Some(error) = &task.error {
            lines.push(format!("工作区异常：{error}"));
        }
    }
    if app.history {
        lines.push(format!(
            "历史已扫描 {} 条 / {} 页 · {}",
            app.snapshot.history_scanned,
            app.snapshot.history_pages,
            if app.snapshot.history_more {
                "n 继续，h 取消历史"
            } else {
                "已到末页"
            }
        ));
        if let Some(error) = &app.snapshot.history_error {
            lines.push(format!("历史读取失败（r 重试）：{error}"));
        }
    }
    if let Some(thread) = sessions.iter().find(|t| Some(&t.id) == app.thread.as_ref()) {
        lines.push(format!("{} · {}", thread.title(), thread.label()));
        lines.push(format!("ID：{}", thread.id));
        lines.push(format!(
            "来源：{}  模型：{}  推理：{}",
            thread.source,
            thread.model.as_deref().unwrap_or("未知"),
            thread.reasoning_effort.as_deref().unwrap_or("未设置")
        ));
        lines.push(format!(
            "创建：{}  更新：{}（北京时间 UTC+8）",
            beijing_time(thread.created_at),
            beijing_time(thread.updated_at)
        ));
        if let Some(parent) = &thread.parent_thread_id {
            lines.push(format!("父会话：{parent}"));
        }
        if app.detail {
            match &app.snapshot.turn {
                Some((id, result)) if id == &thread.id => match result {
                    Ok(text) => lines.extend(text.lines().map(str::to_owned)),
                    Err(error) => lines.push(format!("轮次读取失败：{error}")),
                },
                _ => lines.push("正在读取最近轮次…".into()),
            }
        } else {
            lines.push("在会话区按 Enter 查看最近轮次；不会恢复会话。".into());
        }
    } else {
        lines.push("没有匹配的会话。h 可查看历史；独立 Codex 服务不保证可见。".into());
    }
    frame.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .map(|s| Line::from(safe(&s)))
                .collect::<Vec<_>>(),
        ))
        .block(block(" 详情 · PgUp/PgDn 滚动 ".into(), false))
        .scroll((app.scroll, 0))
        .wrap(Wrap { trim: false }),
        portions[1],
    );
}

fn draw_form(frame: &mut Frame, area: Rect, form: &super::form::Form) {
    if let Some(deletion) = &form.deletion {
        let width = area.width.saturating_sub(4).min(78);
        let height = area.height.saturating_sub(2).min(22);
        let area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
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
            "正在删除，请等待…".into()
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
    let border = block(
        if form.project.is_some() {
            " 新建任务 "
        } else {
            " 登记已有仓库 "
        }
        .into(),
        true,
    );
    let inner = border.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(border, area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(1),
    ])
    .split(inner);
    let labels = if form.project.is_some() {
        ["任务名", "起点分支（已有本地分支）"]
    } else {
        ["已有仓库路径（支持 ~ 和相对路径）", "项目别名"]
    };
    for (i, label) in labels.iter().enumerate() {
        let style = if form.field == i {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{} {label}",
                if form.field == i { "›" } else { " " }
            ))
            .style(style),
            Rect {
                height: 1,
                ..rows[i]
            },
        );
        let value = Line::from(format!(
            "{}{}",
            safe(&form.fields[i]),
            if form.field == i && !form.busy() {
                "▏"
            } else {
                ""
            }
        ));
        let offset = value
            .width()
            .saturating_sub(rows[i].width as usize)
            .min(u16::MAX as usize) as u16;
        frame.render_widget(
            Paragraph::new(value).style(style).scroll((0, offset)),
            Rect {
                y: rows[i].y + 1,
                height: 1,
                ..rows[i]
            },
        );
    }
    frame.render_widget(
        Paragraph::new(if form.busy() {
            "正在执行，请等待…\n执行期间不强制取消，避免中断写操作。"
        } else if form.completing_path() {
            "↑↓ 选择目录 · Tab 补全/切字段\nEnter 确定路径并填别名 · Esc 取消"
        } else if form.project.is_none() {
            "可修改别名，Enter 完成登记 · Esc 取消\nShift+Tab 返回路径 · PgUp/PgDn 滚动错误"
        } else {
            "Tab 切字段 · Enter 确认 · Esc 取消\nPgUp/PgDn 滚动下方说明或错误"
        }),
        rows[2],
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
            .collect();
        list(
            frame,
            rows[3],
            " 本地目录 · Tab 补全 ".into(),
            items,
            Some(form.directory_selection),
            true,
        );
        return;
    }
    let message = match &form.error {
        Some(error) if form.operation_started => format!("操作失败：{}\n已创建的资源会保留；修正输入或处理错误后可重试。", safe(error)),
        Some(error) => format!("请检查输入：{}", safe(error)),
        None => match &form.project {
            Some(project) => format!("项目：{}。创建后留在控制台，Enter 进入 Shell。", safe(project)),
            None if form.completing_path() && form.directory_error.is_some() => format!("路径补全失败：{}", safe(form.directory_error.as_deref().unwrap())),
            None => "登记已有 Git 仓库；未初始化时自动初始化，不克隆、不创建空仓库。已有登记复用原别名。".into(),
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
        rows[3],
    );
}
