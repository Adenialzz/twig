use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use twig_cli::notification::CompletionNotice;

pub struct Completion {
    pub project: String,
    pub path: PathBuf,
    pub task: String,
}

#[derive(Default)]
pub struct Notifications {
    queue: VecDeque<CompletionNotice>,
    current: Option<(CompletionNotice, Instant)>,
    unread: BTreeMap<(String, PathBuf), usize>,
}

impl Notifications {
    pub fn receive(&mut self, event: Completion, viewed: Option<(&str, &Path)>) {
        if viewed.is_some_and(|(project, path)| project == event.project && path == event.path) {
            return;
        }
        *self
            .unread
            .entry((event.project.clone(), event.path))
            .or_default() += 1;
        self.queue.push_back(CompletionNotice {
            project: event.project,
            task: event.task,
        });
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

    pub fn read(&mut self, project: &str, path: &Path) {
        self.unread.remove(&(project.to_owned(), path.to_owned()));
    }

    pub fn count(&self, project: &str, path: Option<&Path>) -> usize {
        self.unread
            .iter()
            .filter(|((p, task), _)| p == project && path.is_none_or(|path| path == task))
            .map(|(_, count)| count)
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
