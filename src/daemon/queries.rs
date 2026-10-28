//! 前台表单的只读业务查询；与状态锁隔离，慢 Git 不阻塞共享快照。
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub fn handle(registry: &Path, method: &str, value: Value) -> Result<Value> {
    match method {
        "form.branches" => Ok(serde_json::to_value(
            crate::console::form::branch_candidates(&serde_json::from_value::<PathBuf>(value)?)?,
        )?),
        "form.ignored" => {
            let path: PathBuf = serde_json::from_value(value)?;
            Ok(json!(if path.try_exists()? {
                crate::git::ignored(&path)?
            } else {
                vec![]
            }))
        }
        "form.project" => {
            let path: PathBuf = serde_json::from_value(value)?;
            ensure!(
                crate::git::git(&path, &["rev-parse", "--is-bare-repository"])? == "false",
                "暂不支持裸仓库"
            );
            Ok(json!(
                crate::git::worktrees(&path)?
                    .first()
                    .context("未找到主工作目录")?
                    .0
            ))
        }
        "form.task" => {
            let (alias, name): (String, String) = serde_json::from_value(value)?;
            let entry = crate::registry::entry(registry, &alias)?;
            let project = crate::config::Project::discover_at(
                &crate::registry::expand(&entry.path)?,
                registry,
            )?;
            ensure!(
                !project.state()?.contains_key(&name),
                "任务 {name} 已存在，请从任务列表进入"
            );
            Ok(json!((
                project.common.clone(),
                crate::existing_branches(&project.config()?, &name)?
            )))
        }
        _ => anyhow::bail!("未知表单查询"),
    }
}

pub fn details(
    mut snapshot: crate::console::data::Snapshot,
    request: crate::console::data::Request,
) -> Result<crate::console::data::Snapshot> {
    let mut client = crate::codex::Client::discover()?;
    if request.history
        && let Some(path) = request.path
    {
        snapshot.history_key = Some((path.clone(), request.descendants));
        let mut cursor = None;
        let mut cursors = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for _ in 0..=request.page {
            let page = client.history(
                if request.descendants {
                    None
                } else {
                    Some(&path)
                },
                cursor.as_deref(),
            )?;
            snapshot.history_pages += 1;
            snapshot.history_scanned += page.data.len();
            for entry in page.data {
                if !entry.ephemeral && ids.insert(entry.id.clone()) {
                    snapshot.history.push(entry);
                }
            }
            cursor = page.next_cursor;
            snapshot.history_more = cursor.is_some();
            if let Some(cursor) = &cursor {
                ensure!(cursors.insert(cursor.clone()), "Codex 返回重复分页游标");
            } else {
                break;
            }
        }
        crate::console::data::associate(&mut snapshot);
        snapshot.history.retain(|entry| {
            snapshot
                .associations
                .get(&entry.id)
                .is_some_and(|(project, task)| {
                    Some(project) == request.project.as_ref() && *task == path
                })
        });
    }
    if let Some(id) = request.turn {
        snapshot.turn = Some((
            id.clone(),
            client.latest_turn(&id).map_err(|e| format!("{e:#}")),
        ));
    }
    Ok(snapshot)
}
