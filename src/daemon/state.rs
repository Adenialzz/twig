//! 单一采集器与客户端关注范围；视图租约不随进程崩溃永久保留。
use crate::console::{
    data::{Request, Snapshot, Worker},
    inbox::{Event, Inbox},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct Presence {
    at: Instant,
    target: Value,
}
static PRESENCE: OnceLock<Mutex<BTreeMap<String, Presence>>> = OnceLock::new();
fn presence() -> &'static Mutex<BTreeMap<String, Presence>> {
    PRESENCE.get_or_init(Default::default)
}
fn matches(target: &Value, event: &Event) -> bool {
    match &event.task {
        None => target == "controller",
        Some(task) => {
            target["project"] == task.project
                && target["path"]
                    .as_str()
                    .is_some_and(|p| Path::new(p) == task.path)
        }
    }
}
pub fn is_viewed(event: &Event) -> bool {
    presence()
        .lock()
        .unwrap()
        .values()
        .any(|p| p.at.elapsed() < Duration::from_secs(10) && matches(&p.target, event))
}
struct Client {
    at: Instant,
    request: Request,
}
pub struct State {
    registry: PathBuf,
    worker: Worker,
    snapshot: Snapshot,
    clients: BTreeMap<String, Client>,
    refresh: u64,
    version: u64,
    instance: String,
}
impl State {
    pub fn start(registry: &Path) -> Result<Self> {
        Ok(Self {
            registry: registry.to_owned(),
            worker: Worker::start_local(registry.to_owned())?,
            snapshot: Snapshot::default(),
            clients: BTreeMap::new(),
            refresh: 0,
            version: 0,
            instance: crate::agent::message_id(),
        })
    }
    fn publish_interest(&self) -> Result<()> {
        let mut projects: Vec<_> = self
            .clients
            .values()
            .filter(|c| !c.request.background)
            .filter_map(|c| c.request.project.clone())
            .collect();
        projects.sort();
        projects.dedup();
        self.worker.requests.send(Request {
            project: projects.first().cloned(),
            background: projects.is_empty(),
            projects,
            refresh: self.refresh,
            ..Default::default()
        })?;
        Ok(())
    }
    pub fn expire(&mut self) {
        let before = self.clients.len();
        self.clients
            .retain(|_, c| c.at.elapsed() < Duration::from_secs(10));
        presence()
            .lock()
            .unwrap()
            .retain(|_, p| p.at.elapsed() < Duration::from_secs(10));
        if before != self.clients.len() {
            let _ = self.publish_interest();
        }
    }
    pub fn health(&mut self) -> Value {
        self.collect();
        serde_json::json!({
            "clients": self.clients.len(), "version": self.version,
            "registry_age_seconds": self.snapshot.registry_checked.map(|t|t.elapsed().as_secs()),
            "registry_error": self.snapshot.registry_error,
            "codex_age_seconds": self.snapshot.codex_checked.map(|t|t.elapsed().as_secs()),
            "codex_error": self.snapshot.codex_error,
            "claude_age_seconds": self.snapshot.claude_checked.map(|t|t.elapsed().as_secs()),
            "claude_error": self.snapshot.claude_error,
            "delivery_error": self.snapshot.delivery_error,
            "projects": self.snapshot.repos.iter().map(|r|serde_json::json!({"name":r.name,"error":r.error,
                "tasks":r.tasks.iter().map(|t|serde_json::json!({"name":t.name,"age_seconds":t.checked.map(|t|t.elapsed().as_secs()),"error":t.error})).collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        })
    }
    fn collect(&mut self) {
        if let Some(snapshot) = self.worker.updates.take() {
            self.snapshot = snapshot;
            self.version += 1;
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }
    pub fn handle(&mut self, method: &str, value: Value) -> Result<Value> {
        let inbox = Inbox::new(&self.registry)?;
        match method {
            "snapshot" => {
                let id = value["client"]
                    .as_str()
                    .context("缺少客户端 ID")?
                    .to_owned();
                let request: Request = serde_json::from_value(value["request"].clone())?;
                if self.clients.get(&id).map_or(request.refresh > 0, |c| {
                    c.request.refresh != request.refresh
                }) {
                    self.refresh += 1;
                }
                self.clients
                    .retain(|_, c| c.at.elapsed() < Duration::from_secs(10));
                ensure!(
                    self.clients.len() < 128 || self.clients.contains_key(&id),
                    "客户端过多"
                );
                self.clients.insert(
                    id.clone(),
                    Client {
                        at: Instant::now(),
                        request,
                    },
                );
                if let Some(p) = presence().lock().unwrap().get_mut(&id) {
                    p.at = Instant::now();
                }
                self.publish_interest()?;
                self.collect();
                Ok(
                    serde_json::json!({"instance":self.instance,"version":self.version,"snapshot":if value["instance"]==self.instance && value["version"].as_u64()==Some(self.version){None}else{Some(&self.snapshot)}}),
                )
            }
            "inbox" => {
                let id = value.as_str().context("缺少客户端 ID")?;
                let state = inbox.load()?;
                let mut presence = presence().lock().unwrap();
                presence.retain(|_, p| p.at.elapsed() < Duration::from_secs(10));
                let p = presence.entry(id.into()).or_insert(Presence {
                    at: Instant::now(),
                    target: Value::Null,
                });
                p.at = Instant::now();
                Ok(serde_json::to_value(state)?)
            }
            "viewing" => {
                let id = value["client"].as_str().context("缺少客户端 ID")?;
                let target = value["target"].clone();
                let mut presence = presence().lock().unwrap();
                let p = presence.entry(id.into()).or_insert(Presence {
                    at: Instant::now(),
                    target: Value::Null,
                });
                p.at = Instant::now();
                p.target = target;
                let ids: HashSet<String> = serde_json::from_value(value["seen"].clone())
                    .context("缺少已查看的事件水位")?;
                inbox.discard(&ids)?;
                Ok(Value::Null)
            }
            "disconnect" => {
                let id = value.as_str().context("缺少客户端 ID")?;
                self.clients.remove(id);
                self.publish_interest()?;
                presence().lock().unwrap().remove(id);
                Ok(Value::Null)
            }
            "record_use" => {
                let (project, task): (String, String) = serde_json::from_value(value)?;
                let entry = crate::registry::entry(&self.registry, &project)?;
                let repository = crate::config::Project::discover_at(
                    &crate::registry::expand(&entry.path)?,
                    &self.registry,
                )?;
                let config = repository.config()?;
                ensure!(
                    crate::tmux::window(&config, &repository, &task)?.is_some(),
                    "窗口已失效"
                );
                crate::registry::record_use(&self.registry, &project, &task)?;
                self.refresh += 1;
                Ok(Value::Null)
            }
            _ => anyhow::bail!("未知状态请求"),
        }
    }
}
