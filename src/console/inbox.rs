//! 未读与采集进度独立于配置；先保存事件，再推进游标，崩溃重试按事件 ID 去重。
use super::notifications::{Completion, Deliveries};
use crate::codex::Turn;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub thread: String,
    pub task: Option<Completion>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::notifications::{ChatNotifications, Notifications};
    use std::time::{Duration, Instant};

    fn event(id: &str, task: bool) -> Event {
        Event {
            id: id.into(),
            thread: if task {
                "task-thread"
            } else {
                "controller-thread"
            }
            .into(),
            task: task.then(|| Completion {
                message: true,
                project: "demo".into(),
                path: "/work".into(),
                task: "work".into(),
            }),
        }
    }

    #[test]
    fn stable_remote_view_is_local_and_only_acknowledges_observed_events() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = Inbox::remote(&temp.path().join("config.toml"), "client".into()).unwrap();
        let remote = inbox.remote.as_ref().unwrap();
        remote.cache.lock().unwrap().target = serde_json::json!({"project":"demo","path":"/work"});
        remote
            .cache
            .lock()
            .unwrap()
            .state
            .unread
            .push(event("old", true));
        assert_eq!(inbox.load().unwrap().unread.len(), 1);
        // 后台拉取了新事件，但 UI 尚未看到，不能随进入任务一起清除。
        remote
            .cache
            .lock()
            .unwrap()
            .state
            .unread
            .push(event("new", true));
        inbox.read_task("demo", Path::new("/work")).unwrap();
        assert!(inbox.viewing(serde_json::Value::Null).is_err());
        let cache = remote.cache.lock().unwrap();
        assert_eq!(cache.read, HashSet::from(["old".into()]));
        assert_eq!(cache.state.unread[0].id, "new");
        // 没有后台也能完成 UI 更新；切走保留待上报水位，不依赖一次 RPC 的时序。
        assert!(cache.target.is_null());
        assert!(!temp.path().join("config.toml").exists());
    }

    #[test]
    fn restart_preserves_unread_and_read_receipts_without_replaying_cards() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        fs::write(&config, "original config").unwrap();
        let inbox = Inbox::new(&config).unwrap();
        inbox.record(event("one", true), false).unwrap();
        inbox.record(event("two", true), false).unwrap();
        inbox.record(event("chat", false), false).unwrap();
        let reopened = Inbox::new(&config).unwrap();
        let saved = reopened.load().unwrap();
        let mut tasks = Notifications::default();
        tasks.restore(&saved);
        let mut chat = ChatNotifications::persisted(reopened.clone(), &saved);
        tasks.tick(Instant::now() + Duration::from_secs(9));
        assert_eq!(tasks.count("demo", None), 2);
        assert!(tasks.current().is_none());
        assert_eq!(chat.status(Instant::now(), true), (true, false));
        reopened.read_task("demo", Path::new("/work")).unwrap();
        chat.set_viewing(true).unwrap();
        // 崩溃发生在事件写入后、游标保存前，重试也不能复活已读事件。
        reopened.record(event("one", true), false).unwrap();
        reopened.record(event("chat", false), false).unwrap();
        assert!(
            Inbox::new(&config)
                .unwrap()
                .load()
                .unwrap()
                .unread
                .is_empty()
        );
        assert_eq!(fs::read_to_string(config).unwrap(), "original config");
        assert!(temp.path().join("notifications.json").is_file());
    }

    #[test]
    fn saved_delivery_baseline_catches_offline_messages_only_once() {
        use crate::agent::{Binding, Delivery, State as AgentState};
        let temp = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(&temp.path().join("config.toml")).unwrap();
        let mut agent = AgentState::default();
        agent.bindings.insert(
            "controller".into(),
            Binding {
                thread: "controller-thread".into(),
                cwd: "/work".into(),
                project: None,
                task: None,
                stopped: false,
            },
        );
        let mut deliveries = Deliveries::default();
        assert!(deliveries.observe(&agent, |_, _| unreachable!()).is_empty());
        let watched = BTreeMap::from([(
            "task-thread".into(),
            (
                PathBuf::from("/work"),
                Some(Turn {
                    id: "running".into(),
                    status: "inProgress".into(),
                }),
            ),
        )]);
        inbox.checkpoint(&deliveries, &watched).unwrap();
        agent.messages.push(Delivery {
            id: "offline".into(),
            thread: "controller-thread".into(),
            text: "不应保存正文".into(),
            source: None,
            status: "sent".into(),
            turn: None,
            error: None,
        });
        let mut restored = inbox.load().unwrap();
        assert_eq!(
            restored.watched["task-thread"].1.as_ref().unwrap().status,
            "inProgress"
        );
        assert!(
            restored
                .deliveries
                .observe(&agent, |_, id| inbox.record(event(id, false), false))
                .is_empty()
        );
        inbox
            .checkpoint(&restored.deliveries, &restored.watched)
            .unwrap();
        let mut again = inbox.load().unwrap();
        assert_eq!(again.unread.len(), 1);
        assert!(
            again
                .deliveries
                .observe(&agent, |_, _| unreachable!())
                .is_empty()
        );
        assert!(
            !fs::read_to_string(&inbox.path)
                .unwrap()
                .contains("不应保存正文")
        );
    }

    #[test]
    fn separate_writers_merge_events_and_read_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(&temp.path().join("config.toml")).unwrap();
        let workers: Vec<_> = (0..2)
            .map(|writer| {
                let inbox = inbox.clone();
                std::thread::spawn(move || {
                    for n in 0..10 {
                        inbox
                            .record(event(&format!("{writer}:{n}"), writer == 0), false)
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(inbox.load().unwrap().unread.len(), 20);
        let other = inbox.clone();
        inbox.read_task("demo", Path::new("/work")).unwrap();
        other.record(event("new", false), false).unwrap();
        assert_eq!(inbox.load().unwrap().unread.len(), 11);
    }

    #[test]
    fn silent_events_and_configuration_isolation_and_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(&temp.path().join("config.toml")).unwrap();
        let other = Inbox::new(&temp.path().join("dev.toml")).unwrap();
        inbox.record(event("viewed", true), true).unwrap();
        inbox.record(event("viewed", true), false).unwrap();
        assert!(inbox.load().unwrap().unread.is_empty());
        other.record(event("separate", true), false).unwrap();
        assert!(inbox.load().unwrap().unread.is_empty());
        assert_eq!(other.load().unwrap().unread.len(), 1);
        fs::write(&inbox.path, "broken").unwrap();
        assert!(inbox.record(event("new", true), false).is_err());
        assert_eq!(fs::read_to_string(&inbox.path).unwrap(), "broken");
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub unread: Vec<Event>,
    seen: HashSet<String>,
    pub deliveries: Deliveries,
    pub watched: BTreeMap<String, (PathBuf, Option<Turn>)>,
}

#[derive(Clone)]
pub struct Inbox {
    path: PathBuf,
    remote: Option<Remote>,
}
#[derive(Clone)]
struct Remote {
    registry: PathBuf,
    client: String,
    cache: Arc<Mutex<RemoteState>>,
    sync_lock: Arc<Mutex<()>>,
}
#[derive(Default)]
struct RemoteState {
    state: State,
    seen: HashSet<String>,
    read: HashSet<String>,
    target: serde_json::Value,
}

impl Inbox {
    pub fn new(registry: &Path) -> Result<Self> {
        let parent = registry.parent().context("登记表无父目录")?;
        let path = if registry.file_name().is_some_and(|n| n == "config.toml") {
            parent.join("notifications.json")
        } else {
            parent
                .join("notifications")
                .join(format!("{}.json", crate::config::repository_id(registry)))
        };
        Ok(Self { path, remote: None })
    }

    pub fn remote(registry: &Path, client: String) -> Result<Self> {
        let mut inbox = Self::new(registry)?;
        inbox.remote = Some(Remote {
            registry: registry.to_owned(),
            client,
            cache: Arc::new(Mutex::new(RemoteState::default())),
            sync_lock: Arc::new(Mutex::new(())),
        });
        Ok(inbox)
    }
    pub fn client_id(&self) -> Option<&str> {
        self.remote.as_ref().map(|remote| remote.client.as_str())
    }
    // 仅订阅线程调用网络；终端按键和绘制只读缓存，不在每轮 UI 回调等待 RPC。
    pub fn sync(&self) -> Result<()> {
        let Some(remote) = &self.remote else {
            return Ok(());
        };
        let _sync = remote.sync_lock.lock().unwrap();
        let (target, read) = {
            let cache = remote.cache.lock().unwrap();
            (cache.target.clone(), cache.read.clone())
        };
        let _: serde_json::Value = crate::daemon::call(
            &remote.registry,
            "viewing",
            serde_json::json!({"client":remote.client,"target":target,"seen":read}),
        )?;
        let state: State =
            crate::daemon::call(&remote.registry, "inbox", serde_json::json!(remote.client))?;
        let mut cache = remote.cache.lock().unwrap();
        cache.read.retain(|id| !read.contains(id));
        cache.state = state;
        let pending = cache.read.clone();
        cache
            .state
            .unread
            .retain(|event| !pending.contains(&event.id));
        Ok(())
    }
    pub fn viewing(&self, target: serde_json::Value) -> Result<()> {
        if let Some(remote) = &self.remote {
            let mut cache = remote.cache.lock().unwrap();
            let changed = cache.target != target;
            let read: HashSet<_> = cache
                .state
                .unread
                .iter()
                .filter(|event| {
                    cache.seen.contains(&event.id)
                        && match &event.task {
                            None => target == "controller",
                            Some(task) => {
                                target["project"] == task.project
                                    && target["path"]
                                        .as_str()
                                        .is_some_and(|p| Path::new(p) == task.path)
                            }
                        }
                })
                .map(|event| event.id.clone())
                .collect();
            cache.state.unread.retain(|event| !read.contains(&event.id));
            cache.read.extend(read);
            cache.target = target;
            drop(cache);
            // 目标切换只确认一次，避免离开页面后的完成事件仍按旧租约静默。
            // 相同目标的逐帧更新完全本地；后台轮询与切换同步按客户端串行。
            if changed {
                self.sync()?;
            }
        }
        Ok(())
    }
    pub fn load(&self) -> Result<State> {
        if let Some(remote) = &self.remote {
            let mut cache = remote.cache.lock().unwrap();
            cache.seen = cache
                .state
                .unread
                .iter()
                .map(|event| event.id.clone())
                .collect();
            return Ok(cache.state.clone());
        }
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("未读状态损坏：{}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("无法读取未读状态：{}", self.path.display())),
        }
    }

    fn update(&self, change: impl FnOnce(&mut State)) -> Result<()> {
        let parent = self.path.parent().unwrap();
        fs::create_dir_all(parent)?;
        // flock 随文件描述符关闭释放；进程异常退出不会遗留不可用的锁。
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.path.with_extension("lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
            "无法锁定未读状态：{}",
            std::io::Error::last_os_error()
        );
        let mut state = self.load()?;
        let before = serde_json::to_value(&state)?;
        change(&mut state);
        if serde_json::to_value(&state)? != before {
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(&mut file, &state)?;
            file.as_file().sync_all()?;
            file.persist(&self.path)?;
        }
        Ok(())
    }

    pub fn record(&self, event: Event, viewed: bool) -> Result<()> {
        let viewed = viewed || crate::daemon::state::is_viewed(&event);
        self.update(|state| {
            let fresh = state.seen.insert(event.id.clone());
            if viewed {
                state.unread.retain(|old| old.id != event.id);
            } else if fresh {
                state.unread.push(event);
            }
        })
    }

    pub fn discard(&self, ids: &HashSet<String>) -> Result<()> {
        if !ids.is_empty() {
            self.update(|state| state.unread.retain(|event| !ids.contains(&event.id)))?;
        }
        Ok(())
    }

    pub fn read_task(&self, project: &str, path: &Path) -> Result<()> {
        if self.remote.is_some() {
            return self.viewing(serde_json::json!({"project":project,"path":path}));
        }
        if !self.load()?.unread.iter().any(|event| {
            event
                .task
                .as_ref()
                .is_some_and(|task| task.project == project && task.path == path)
        }) {
            return Ok(());
        }
        self.update(|state| {
            state.unread.retain(|event| {
                !event
                    .task
                    .as_ref()
                    .is_some_and(|task| task.project == project && task.path == path)
            });
        })
    }

    pub fn read_controller(&self) -> Result<()> {
        if self.remote.is_some() {
            return self.viewing(serde_json::json!("controller"));
        }
        self.update(|state| state.unread.retain(|event| event.task.is_some()))
    }

    pub fn controller(&self, thread: Option<&str>) -> Result<()> {
        self.update(|state| {
            state
                .unread
                .retain(|event| event.task.is_some() || Some(event.thread.as_str()) == thread);
        })
    }

    pub fn checkpoint(
        &self,
        deliveries: &Deliveries,
        watched: &BTreeMap<String, (PathBuf, Option<Turn>)>,
    ) -> Result<()> {
        self.update(|state| {
            state.deliveries.initialized |= deliveries.initialized;
            state
                .deliveries
                .seen
                .extend(deliveries.seen.iter().cloned());
            state.watched.extend(watched.clone());
        })
    }
}
