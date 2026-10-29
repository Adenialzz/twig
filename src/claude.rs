//! Claude 只读状态观察；不恢复会话、不发送输入，也不把状态变化当作轮次完成。
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, process::Command};
use twig_cli::notification::ClaudeState;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub cwd: PathBuf,
    pub kind: String,
    pub started_at: u64,
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub state: Option<String>,
    pub waiting_for: Option<String>,
}

impl Session {
    pub fn key(&self) -> Option<String> {
        // 不使用 PID 作为会话身份；后台尚未分配 UUID 时用其 job ID。
        self.session_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .map(|id| format!("session:{id}"))
            .or_else(|| {
                self.id
                    .as_ref()
                    .filter(|id| !id.is_empty())
                    .map(|id| format!("job:{id}"))
            })
    }

    pub fn status(&self) -> Option<ClaudeState> {
        // 活跃进程的状态优先，避免后台上一轮的 done 掩盖新的 busy。
        match self.status.as_deref() {
            Some("busy") => Some(ClaudeState::Busy),
            Some("waiting") => Some(ClaudeState::Waiting),
            Some("idle") | None => match self.state.as_deref() {
                Some("working") => Some(ClaudeState::Busy),
                Some("blocked") => Some(ClaudeState::Waiting),
                Some("done") => Some(ClaudeState::Done),
                Some("failed") => Some(ClaudeState::Failed),
                Some("stopped") => Some(ClaudeState::Stopped),
                None if self.status.as_deref() == Some("idle") => Some(ClaudeState::Idle),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        let label = self.status().map_or("未知", ClaudeState::label);
        if self.status() == Some(ClaudeState::Waiting)
            && let Some(reason) = &self.waiting_for
        {
            return format!("{label} · {reason}");
        }
        label.into()
    }

    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self.session_id.as_deref())
            .or(self.id.as_deref())
            .unwrap_or("未命名会话")
    }

    pub fn priority(&self) -> u8 {
        match self.status() {
            Some(ClaudeState::Waiting) => 0,
            Some(ClaudeState::Failed) => 1,
            Some(ClaudeState::Busy) => 2,
            _ => 3,
        }
    }
}

pub fn query() -> Result<Vec<Session>> {
    read(&mut Command::new("claude"))
}

fn read(command: &mut Command) -> Result<Vec<Session>> {
    let output = crate::console::query::output(command.args(["agents", "--json", "--all"]))
        .context("无法查询 Claude；需要支持 agents --json 的 Claude Code")?;
    ensure!(
        output.status.success(),
        "Claude 状态查询失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let sessions: Vec<Session> = serde_json::from_slice(&output.stdout)
        .context("Claude 状态格式错误；需要 agents --json 返回会话数组")?;
    let mut keys = std::collections::HashSet::new();
    for session in &sessions {
        ensure!(session.cwd.is_absolute(), "Claude 返回了非绝对工作目录");
        if let Some(key) = session.key() {
            ensure!(keys.insert(key), "Claude 返回了重复会话身份");
        }
    }
    Ok(sessions)
}

#[derive(Default)]
pub struct Observer {
    previous: BTreeMap<String, (PathBuf, u64, Option<ClaudeState>)>,
    generation: String,
    sequence: u64,
}

impl Observer {
    pub fn reset(&mut self) {
        self.previous.clear();
    }

    /// 仅比较连续成功采集。首次发现、重启、失联或消失均不推断完成。
    /// 写入成功后才推进本会话水位，失败重试使用相同事件 ID。
    pub fn observe(
        &mut self,
        sessions: &[Session],
        mut emit: impl FnMut(&Session, ClaudeState, &str) -> Result<()>,
    ) -> Result<()> {
        if self.generation.is_empty() {
            self.generation = crate::agent::message_id();
        }
        let present: std::collections::HashSet<_> =
            sessions.iter().filter_map(Session::key).collect();
        self.previous.retain(|key, _| present.contains(key));
        for session in sessions {
            let Some(key) = session.key() else { continue };
            let current = (session.cwd.clone(), session.started_at, session.status());
            if let Some((cwd, started, Some(before))) = self.previous.get(&key)
                && cwd == &current.0
                && *started == current.1
                && let Some(after) = current.2
                && *before != after
                && after != ClaudeState::Busy
            {
                let id = format!("claude-state:{}:{}", self.generation, self.sequence);
                emit(session, after, &id)?;
                self.sequence += 1;
            }
            self.previous.insert(key, current);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::{
        inbox::{Event, Inbox},
        notifications::{Completion, Notifications},
    };
    use serde_json::json;

    fn session(status: &str) -> Session {
        serde_json::from_value(json!({"cwd":"/work", "kind":"interactive", "startedAt":1,
            "sessionId":"one", "status":status}))
        .unwrap()
    }

    #[test]
    fn current_process_state_wins_and_unknown_is_not_idle() {
        let mut s = session("busy");
        s.state = Some("done".into());
        assert_eq!(s.status(), Some(ClaudeState::Busy));
        s.status = Some("waiting".into());
        s.waiting_for = Some("permission prompt".into());
        assert!(s.label().contains("permission prompt"));
        s.status = Some("new-state".into());
        assert_eq!(s.status(), None);
        s.status = None;
        assert_eq!(s.status(), Some(ClaudeState::Done));
        s.state = Some("new-state".into());
        assert_eq!(s.status(), None);
    }

    #[test]
    fn baselines_disappearance_restart_and_errors_never_notify() {
        let mut observer = Observer::default();
        let mut events = Vec::new();
        let mut observe = |o: &mut Observer, sessions: &[Session]| {
            o.observe(sessions, |_, state, _| {
                events.push(state);
                Ok(())
            })
            .unwrap();
        };
        observe(&mut observer, &[session("idle")]);
        observe(&mut observer, &[session("busy")]);
        observe(&mut observer, &[session("waiting")]);
        observe(&mut observer, &[session("waiting")]);
        observe(&mut observer, &[session("busy")]);
        observe(&mut observer, &[session("idle")]);
        observe(&mut observer, &[session("busy")]);
        observe(&mut observer, &[]);
        observe(&mut observer, &[session("idle")]);
        observe(&mut observer, &[session("busy")]);
        observer.reset();
        observe(&mut observer, &[session("idle")]);
        observe(&mut observer, &[session("busy")]);
        let mut restarted = session("idle");
        restarted.started_at = 2;
        observe(&mut observer, &[restarted]);
        assert_eq!(events, vec![ClaudeState::Waiting, ClaudeState::Idle]);
    }

    #[test]
    fn failed_persistence_retries_same_id_and_unread_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let inbox = Inbox::new(&temp.path().join("config.toml")).unwrap();
        let mut observer = Observer::default();
        observer
            .observe(&[session("busy")], |_, _, _| unreachable!())
            .unwrap();
        let mut ids = Vec::new();
        assert!(
            observer
                .observe(&[session("idle")], |_, _, id| {
                    ids.push(id.to_owned());
                    anyhow::bail!("disk unavailable")
                })
                .is_err()
        );
        observer
            .observe(&[session("idle")], |_, state, id| {
                ids.push(id.to_owned());
                inbox.record(
                    Event {
                        id: id.into(),
                        thread: "claude:session:one".into(),
                        task: Some(Completion {
                            claude: Some(state),
                            message: false,
                            project: "demo".into(),
                            task: "work".into(),
                            path: "/work".into(),
                        }),
                    },
                    false,
                )
            })
            .unwrap();
        assert_eq!(ids[0], ids[1]);
        observer
            .observe(&[session("idle")], |_, _, _| unreachable!())
            .unwrap();
        let saved = Inbox::new(&temp.path().join("config.toml"))
            .unwrap()
            .load()
            .unwrap();
        assert_eq!(saved.unread.len(), 1);
        let mut notices = Notifications::default();
        notices.restore(&saved);
        assert_eq!(notices.count("demo", None), 1);
        assert!(notices.current().is_none());
        inbox
            .read_task("demo", std::path::Path::new("/work"))
            .unwrap();
        assert!(inbox.load().unwrap().unread.is_empty());
        Observer::default()
            .observe(&[session("idle")], |_, _, _| unreachable!())
            .unwrap();
    }

    #[test]
    fn multiple_sessions_and_background_terminal_states_are_distinct() {
        let mut observer = Observer::default();
        let a = session("busy");
        let mut b = a.clone();
        b.session_id = Some("two".into());
        observer
            .observe(&[a.clone(), b.clone()], |_, _, _| unreachable!())
            .unwrap();
        let mut failed = a;
        failed.status = None;
        failed.state = Some("failed".into());
        b.status = None;
        b.state = Some("done".into());
        let mut events = Vec::new();
        observer
            .observe(&[failed, b], |s, state, id| {
                events.push((s.key(), state, id.to_owned()));
                Ok(())
            })
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1, ClaudeState::Failed);
        assert_eq!(events[1].1, ClaudeState::Done);
        assert_ne!(events[0].2, events[1].2);
    }

    #[test]
    fn cli_contract_is_readonly_and_rejects_bad_output_and_timeout() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            sync::{Arc, atomic::AtomicBool},
        };
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("claude");
        fs::write(
            &executable,
            "#!/bin/sh\n[ \"$*\" = 'agents --json --all' ] || exit 9\nprintf '%s' \"$REPLY\"\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let reply = serde_json::to_string(&vec![session("busy")]).unwrap();
        assert_eq!(
            read(Command::new(&executable).env("REPLY", reply))
                .unwrap()
                .len(),
            1
        );
        for reply in [
            "not json",
            "{}",
            "[{\"cwd\":\"relative\",\"kind\":\"interactive\",\"startedAt\":1}]",
        ] {
            assert!(read(Command::new(&executable).env("REPLY", reply)).is_err());
        }
        fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
        let result = std::thread::spawn(move || {
            crate::console::query::install(Arc::new(AtomicBool::new(false)));
            read(&mut Command::new(executable))
        })
        .join()
        .unwrap();
        assert!(format!("{:#}", result.unwrap_err()).contains("超过 2 秒"));
    }
}
