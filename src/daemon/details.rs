//! 慢历史查询独立串行执行；RPC 只提交或读取结果，不占住请求线程。
use crate::console::data::{Request, Snapshot};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
struct Entry {
    key: Value,
    result: Option<std::result::Result<Snapshot, String>>,
    touched: Instant,
}
struct Job {
    client: String,
    snapshot: Snapshot,
    request: Request,
}
pub(crate) struct Details {
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    queue: Option<mpsc::SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
}
impl Details {
    pub fn start(stop: Arc<AtomicBool>) -> Self {
        Self::with_query(move |snapshot, request| {
            crate::console::query::install(stop.clone());
            super::queries::details(snapshot, request)
        })
    }
    fn with_query(
        mut query: impl FnMut(Snapshot, Request) -> Result<Snapshot> + Send + 'static,
    ) -> Self {
        let entries = Arc::new(Mutex::new(BTreeMap::<String, Entry>::new()));
        let results = entries.clone();
        let (queue, input) = mpsc::sync_channel::<Job>(64);
        let worker = thread::spawn(move || {
            for job in input {
                let result = query(job.snapshot, job.request).map_err(|error| format!("{error:#}"));
                let mut entries = results.lock().unwrap();
                let entry = entries.get_mut(&job.client).unwrap();
                entry.result = Some(result);
                entry.touched = Instant::now();
            }
        });
        Self {
            entries,
            queue: Some(queue),
            worker: Some(worker),
        }
    }
    pub fn poll(
        &self,
        client: String,
        request: Request,
        snapshot: impl FnOnce() -> Snapshot,
    ) -> Result<Option<Snapshot>> {
        let key = serde_json::to_value(&request)?;
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, entry| {
            entry.result.is_none() || entry.touched.elapsed() < Duration::from_secs(30)
        });
        if let Some(entry) = entries.get_mut(&client) {
            // 切换目标也等待该客户端旧请求结束，避免产生无限的废弃查询。
            if entry.result.is_none() {
                return Ok(None);
            }
            if entry.key == key {
                match entry.result.as_ref().unwrap() {
                    Ok(snapshot) => {
                        let failed = snapshot.history_error.is_some()
                            || snapshot
                                .turn
                                .as_ref()
                                .is_some_and(|(_, result)| result.is_err());
                        if !failed || entry.touched.elapsed() < Duration::from_secs(2) {
                            if !failed {
                                entry.touched = Instant::now();
                            }
                            return Ok(Some(snapshot.clone()));
                        }
                    }
                    Err(error) if entry.touched.elapsed() < Duration::from_secs(2) => {
                        anyhow::bail!("{error}")
                    }
                    Err(_) => {}
                }
            }
        }
        ensure!(
            entries.contains_key(&client) || entries.len() < 64,
            "历史查询队列已满，请稍后重试"
        );
        self.queue
            .as_ref()
            .unwrap()
            .try_send(Job {
                client: client.clone(),
                request,
                snapshot: snapshot(),
            })
            .context("无法提交历史查询")?;
        entries.insert(
            client,
            Entry {
                key,
                result: None,
                touched: Instant::now(),
            },
        );
        Ok(None)
    }
}
impl Drop for Details {
    fn drop(&mut self) {
        self.queue.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_queries_are_deduplicated_and_do_not_block_polls() {
        let (entered, started) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let details = Details::with_query(move |snapshot, _| {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            Ok(snapshot)
        });
        assert!(
            details
                .poll("a".into(), Request::default(), Snapshot::default)
                .unwrap()
                .is_none()
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let now = Instant::now();
        for _ in 0..20 {
            assert!(
                details
                    .poll("a".into(), Request::default(), Snapshot::default)
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            details
                .poll("b".into(), Request::default(), Snapshot::default)
                .unwrap()
                .is_none()
        );
        assert!(now.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            details
                .poll("a".into(), Request::default(), Snapshot::default)
                .unwrap()
                .is_some()
        );
        release.send(()).unwrap();
        drop(details);
        assert!(started.try_recv().is_err(), "重复轮询不能重复执行查询");
    }
}
