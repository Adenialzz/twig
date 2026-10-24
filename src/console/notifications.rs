use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use twig_cli::notification::{CompletionNotice, UnreadTask};

/// 首次成功读取只建立基线；待发消息确认送达后才消耗其 ID。
#[derive(Default)]
pub struct Deliveries {
    initialized: bool,
    seen: std::collections::HashSet<String>,
}
impl Deliveries {
    pub fn observe<'a>(
        &mut self,
        state: &'a crate::agent::State,
    ) -> Vec<&'a crate::agent::Binding> {
        let mut recipients = Vec::new();
        for message in state.messages.iter().filter(|m| m.status == "sent") {
            if self.seen.insert(message.id.clone())
                && self.initialized
                && let Some(binding) = state.bindings.values().find(|b| b.thread == message.thread)
            {
                recipients.push(binding);
            }
        }
        self.initialized = true;
        recipients
    }
}

pub struct Completion {
    pub message: bool,
    pub project: String,
    pub path: PathBuf,
    pub task: String,
}

struct Unread {
    task: UnreadTask,
    latest: u64,
}

#[derive(Default)]
pub struct Notifications {
    queue: VecDeque<CompletionNotice>,
    current: Option<(CompletionNotice, Instant)>,
    unread: BTreeMap<(String, PathBuf), Unread>,
    sequence: u64,
}

impl Notifications {
    pub fn receive(&mut self, event: Completion, viewed: Option<(&str, &Path)>) {
        if viewed.is_some_and(|(project, path)| project == event.project && path == event.path) {
            return;
        }
        self.sequence += 1;
        let notice = CompletionNotice {
            message: event.message,
            project: event.project.clone(),
            task: event.task,
        };
        let unread = self
            .unread
            .entry((event.project, event.path))
            .or_insert_with(|| Unread {
                task: UnreadTask {
                    notice: notice.clone(),
                    count: 0,
                },
                latest: self.sequence,
            });
        unread.task.count += 1;
        unread.latest = self.sequence;
        self.queue.push_back(notice);
    }

    pub fn tick(&mut self, now: Instant) {
        if self
            .current
            .as_ref()
            .is_some_and(|(_, since)| now.duration_since(*since) >= Duration::from_secs(8))
        {
            self.current = None;
        }
        if self.current.is_none() {
            self.current = self.queue.pop_front().map(|message| (message, now));
        }
    }

    pub fn current(&self) -> Option<&CompletionNotice> {
        self.current.as_ref().map(|(message, _)| message)
    }

    pub fn dismiss(&mut self, notice: &CompletionNotice) {
        if self.current() == Some(notice) {
            self.current = None;
            self.tick(Instant::now());
        }
    }

    pub fn read(&mut self, project: &str, path: &Path) {
        self.unread.remove(&(project.to_owned(), path.to_owned()));
    }

    pub fn tasks(&self) -> Vec<UnreadTask> {
        let mut unread = self.unread.values().collect::<Vec<_>>();
        unread.sort_by_key(|item| std::cmp::Reverse(item.latest));
        unread.into_iter().map(|item| item.task.clone()).collect()
    }

    pub fn count(&self, project: &str, path: Option<&Path>) -> usize {
        self.unread
            .iter()
            .filter(|((p, task), _)| p == project && path.is_none_or(|path| path == task))
            .map(|(_, unread)| unread.task.count)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queues_messages_and_only_clears_the_viewed_task() {
        let mut notices = Notifications::default();
        for (project, task) in [("a", "one"), ("a", "two"), ("b", "one")] {
            notices.receive(
                Completion {
                    message: false,
                    project: project.into(),
                    path: format!("/{project}/{task}").into(),
                    task: task.into(),
                },
                None,
            );
        }
        let now = Instant::now();
        notices.tick(now);
        assert!(
            notices.current().unwrap().project == "a" && notices.current().unwrap().task == "one"
        );
        notices.read("a", Path::new("/a/one"));
        assert_eq!(notices.count("a", None), 1);
        assert_eq!(notices.count("b", None), 1);
        notices.tick(now + Duration::from_secs(7));
        assert!(
            notices.current().unwrap().project == "a" && notices.current().unwrap().task == "one"
        );
        notices.tick(now + Duration::from_secs(8));
        assert!(notices.current().unwrap().task == "two");
        notices.tick(now + Duration::from_secs(16));
        assert!(notices.current().unwrap().project == "b");
        notices.tick(now + Duration::from_secs(24));
        assert!(notices.current().is_none());
        assert_eq!(notices.count("a", Some(Path::new("/a/two"))), 1);
    }

    #[test]
    fn unread_tasks_merge_sort_and_survive_card_expiry() {
        let mut notices = Notifications::default();
        for (project, task) in [("a", "one"), ("b", "two"), ("a", "one")] {
            notices.receive(
                Completion {
                    message: false,
                    project: project.into(),
                    path: format!("/{project}/{task}").into(),
                    task: task.into(),
                },
                None,
            );
        }
        let tasks = notices.tasks();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].notice.task, "one");
        assert_eq!(tasks[0].count, 2);
        for i in 0..4 {
            notices.tick(Instant::now() + Duration::from_secs(i * 8));
        }
        assert!(notices.current().is_none());
        assert_eq!(notices.tasks(), tasks);
        notices.read("a", Path::new("/a/one"));
        assert_eq!(notices.tasks(), tasks[1..]);
    }

    #[test]
    fn dismiss_advances_queue_without_clearing_other_unread() {
        let mut notices = Notifications::default();
        for task in ["one", "two"] {
            notices.receive(
                Completion {
                    message: false,
                    project: "demo".into(),
                    path: format!("/{task}").into(),
                    task: task.into(),
                },
                None,
            );
        }
        notices.tick(Instant::now());
        let first = notices.current().unwrap().clone();
        notices.dismiss(&first);
        notices.read("demo", Path::new("/one"));
        assert_eq!(notices.current().unwrap().task, "two");
        assert_eq!(notices.count("demo", None), 1);
        notices.dismiss(&first);
        assert_eq!(notices.current().unwrap().task, "two");
        let second = notices.current().unwrap().clone();
        notices.dismiss(&second);
        assert!(notices.current().is_none());
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    #[test]
    fn only_the_actual_viewed_task_is_silent() {
        for (viewed, expected) in [
            (Some(("demo", Path::new("/task"))), 0),
            (Some(("demo", Path::new("/other"))), 1),
            (Some(("other", Path::new("/task"))), 1),
            (None, 1),
        ] {
            let mut notices = Notifications::default();
            notices.receive(
                Completion {
                    message: false,
                    project: "demo".into(),
                    path: "/task".into(),
                    task: "task".into(),
                },
                viewed,
            );
            notices.tick(Instant::now());
            assert_eq!(notices.count("demo", None), expected);
            assert_eq!(notices.current().is_some(), expected == 1);
        }
    }
}

/// 与采集线程共享，按事件发生时所在页面决定是否提醒。
#[derive(Default)]
pub struct ChatNotifications {
    viewing: bool,
    unread: bool,
    pending: bool,
    since: Option<Instant>,
}
impl ChatNotifications {
    pub fn viewing(&mut self, viewing: bool) {
        self.viewing = viewing;
        if viewing {
            self.unread = false;
            self.pending = false;
            self.since = None;
        }
    }
    pub fn receive(&mut self) {
        if !self.viewing {
            self.unread = true;
            self.pending = true;
            self.since = None;
        }
    }
    pub fn status(&mut self, now: Instant, can_show: bool) -> (bool, bool) {
        if self.pending && can_show {
            let since = self.since.get_or_insert(now);
            if now.duration_since(*since) >= Duration::from_secs(8) {
                self.pending = false;
                self.since = None;
            }
        }
        (self.unread, self.pending && can_show)
    }
}

#[cfg(test)]
mod chat_tests {
    use super::*;

    #[test]
    fn chat_is_silent_while_viewing_and_unread_survives_card_expiry() {
        let mut chat = ChatNotifications::default();
        let now = Instant::now();
        chat.viewing(true);
        chat.receive();
        assert_eq!(chat.status(now, true), (false, false));
        chat.viewing(false);
        chat.receive();
        assert_eq!(chat.status(now, false), (true, false));
        // 任务卡片先显示，主控卡片从实际显示时开始计时。
        let shown = now + Duration::from_secs(20);
        assert_eq!(chat.status(shown, true), (true, true));
        assert_eq!(
            chat.status(shown + Duration::from_secs(8), true),
            (true, false)
        );
        chat.receive();
        chat.viewing(true);
        assert_eq!(chat.status(shown, true), (false, false));
        chat.viewing(false);
        assert_eq!(chat.status(shown, true), (false, false));
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use crate::agent::{Binding, Delivery, State};

    fn message(id: &str, thread: &str, status: &str) -> Delivery {
        Delivery {
            id: id.into(),
            thread: thread.into(),
            text: "相同正文".into(),
            status: status.into(),
            turn: None,
            error: None,
        }
    }

    #[test]
    fn delivery_baseline_transitions_and_recipient_visibility() {
        let mut state = State::default();
        for (key, thread, project, task) in [
            ("controller", "controller-id", None, None),
            (
                "task-id",
                "task-id",
                Some("demo".into()),
                Some("work".into()),
            ),
        ] {
            state.bindings.insert(
                key.into(),
                Binding {
                    thread: thread.into(),
                    cwd: "/work".into(),
                    project,
                    task,
                    stopped: false,
                },
            );
        }
        state.messages = vec![
            message("history", "controller-id", "sent"),
            message("pending", "task-id", "queued"),
        ];
        let mut deliveries = Deliveries::default();
        assert!(deliveries.observe(&state).is_empty());
        state.messages.extend([
            message("unknown", "controller-id", "sending"),
            message("failed", "task-id", "failed"),
            message("cancelled", "task-id", "cancelled"),
        ]);
        assert!(deliveries.observe(&state).is_empty());
        state.messages[1].status = "sent".into();
        state.messages[2].status = "sent".into();
        let recipients = deliveries.observe(&state);
        assert_eq!(
            recipients
                .iter()
                .map(|b| b.thread.as_str())
                .collect::<Vec<_>>(),
            ["task-id", "controller-id"]
        );
        let mut tasks = Notifications::default();
        let mut chat = ChatNotifications::default();
        chat.viewing(true);
        for binding in recipients {
            if let (Some(project), Some(task)) = (&binding.project, &binding.task) {
                tasks.receive(
                    Completion {
                        message: true,
                        project: project.clone(),
                        task: task.clone(),
                        path: binding.cwd.clone(),
                    },
                    None,
                );
            } else {
                chat.receive();
            }
        }
        tasks.tick(Instant::now());
        assert!(tasks.current().unwrap().message);
        assert_eq!(tasks.count("demo", None), 1);
        assert_eq!(chat.status(Instant::now(), true), (false, false));
        assert!(deliveries.observe(&state).is_empty());
        tasks.read("demo", Path::new("/work"));
        tasks.receive(
            Completion {
                message: true,
                project: "demo".into(),
                task: "work".into(),
                path: "/work".into(),
            },
            Some(("demo", Path::new("/work"))),
        );
        assert_eq!(tasks.count("demo", None), 0);
        state
            .messages
            .push(message("same-text-new-id", "controller-id", "sent"));
        chat.viewing(false);
        for _ in deliveries.observe(&state) {
            chat.receive();
        }
        assert_eq!(chat.status(Instant::now(), true), (true, true));
        chat.viewing(true);
        assert_eq!(chat.status(Instant::now(), true), (false, false));
    }

    #[test]
    fn empty_baseline_and_replaced_bindings_do_not_replay_old_messages() {
        let mut state = State::default();
        let mut deliveries = Deliveries::default();
        assert!(deliveries.observe(&state).is_empty());
        state.bindings.insert(
            "controller".into(),
            Binding {
                thread: "new".into(),
                cwd: "/work".into(),
                project: None,
                task: None,
                stopped: false,
            },
        );
        state.messages.push(message("old", "old", "sent"));
        state.messages.push(message("new", "new", "sent"));
        assert_eq!(deliveries.observe(&state).len(), 1);
        assert!(deliveries.observe(&state).is_empty());
    }
}
