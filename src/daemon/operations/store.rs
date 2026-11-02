//! 单操作原子存储。旧整表只迁移一次，完整目录发布后不再读取旧文件。
use super::Operation;
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct Store {
    pub(crate) directory: PathBuf,
}
impl Store {
    fn initialize(root: &Path) -> Result<Self> {
        let directory = root.join("operations");
        if !directory.try_exists()? {
            let legacy = root.join("operations.json");
            let records: BTreeMap<String, Operation> = match fs::read(&legacy) {
                Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                    format!("操作记录损坏，保留文件并停止启动：{}", legacy.display())
                })?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.into()),
            };
            // 未发布的临时目录不参与恢复；中途退出后从原文件重新迁移。
            let staging = tempfile::Builder::new()
                .prefix(".operations-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(root)?;
            let store = Self {
                directory: staging.path().to_owned(),
            };
            for (id, record) in &records {
                ensure!(id == &record.id, "旧操作记录的键与 ID 不一致：{id}");
                store.save(record)?;
            }
            fs::File::open(staging.path())?.sync_all()?;
            fs::rename(staging.path(), &directory)?;
            fs::File::open(root)?.sync_all()?;
        }
        Ok(Self { directory })
    }
    pub(crate) fn open(root: &Path) -> Result<(Self, BTreeMap<String, Operation>)> {
        let store = Self::initialize(root)?;
        let records = Self::read(root)?;
        for record in records.values() {
            store.archive(record)?;
        }
        Ok((store, records))
    }
    pub(crate) fn open_active(root: &Path) -> Result<(Self, BTreeMap<String, Operation>)> {
        let store = Self::initialize(root)?;
        let mut records = Self::read_active(root)?;
        for record in records.values() {
            store.archive(record)?;
        }
        records.retain(|_, record| !finished(record));
        Ok((store, records))
    }
    pub(crate) fn read(root: &Path) -> Result<BTreeMap<String, Operation>> {
        let active = Self::read_active(root)?;
        let archive = root.join("operations/archive");
        let mut records = if archive.try_exists()? {
            Self::read_directory(&archive)?
        } else {
            BTreeMap::new()
        };
        // 终态归档保存后才移除活动记录；若移除失败，活动记录继续阻止误放行。
        records.extend(active);
        Ok(records)
    }
    /// 只读查询不迁移、不创建目录；新目录存在时绝不回退到旧备份。
    pub(crate) fn read_active(root: &Path) -> Result<BTreeMap<String, Operation>> {
        let directory = root.join("operations");
        if !directory.try_exists()? {
            let legacy = root.join("operations.json");
            return match fs::read(&legacy) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .with_context(|| format!("操作记录损坏：{}", legacy.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
                Err(error) => Err(error.into()),
            };
        }
        Self::read_directory(&directory)
    }
    fn read_directory(directory: &Path) -> Result<BTreeMap<String, Operation>> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(directory)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "操作目录必须属于当前用户且权限为 0700：{}",
            directory.display()
        );
        let store = Self {
            directory: directory.to_owned(),
        };
        let mut records = BTreeMap::new();
        for entry in fs::read_dir(&store.directory)? {
            let entry = entry?;
            let path = entry.path();
            // NamedTempFile 在崩溃时可能留下未发布的临时文件。
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let moved = if directory.file_name().is_some_and(|name| name == "archive") {
                directory.parent().unwrap().join(entry.file_name())
            } else {
                directory.join("archive").join(entry.file_name())
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::symlink_metadata(&moved)?.file_type()
                }
                Err(error) => return Err(error.into()),
            };
            ensure!(kind.is_file(), "操作记录不是普通文件：{}", path.display());
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // 完成记录可能在列目录之后原子移入 archive。
                    fs::read(&moved)?
                }
                Err(error) => return Err(error.into()),
            };
            let record: Operation = serde_json::from_slice(&bytes)
                .with_context(|| format!("操作记录损坏，保留文件并停止启动：{}", path.display()))?;
            ensure!(
                store.path(&record.id)?.file_name() == path.file_name(),
                "操作记录文件名与 ID 不一致：{}",
                path.display()
            );
            records.insert(record.id.clone(), record);
        }
        Ok(records)
    }
    pub(crate) fn path(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
            "操作 ID 不能用作记录文件名：{id}"
        );
        let archived = self.directory.join("archive").join(format!("{id}.json"));
        let active = self.directory.join(format!("{id}.json"));
        Ok(if active.try_exists()? || !archived.try_exists()? {
            active
        } else {
            archived
        })
    }
    pub(crate) fn save(&self, record: &Operation) -> Result<()> {
        self.path(&record.id)?; // 校验 ID，不将外部输入拼成任意路径。
        let active = self.directory.join(format!("{}.json", record.id));
        let archive = self.directory.join("archive");
        let archived = archive.join(format!("{}.json", record.id));
        let path = if finished(record) {
            super::super::private_directory(&archive)?;
            &archived
        } else {
            &active
        };
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            serde_json::to_writer(&mut writer, record)?;
            writer.flush()?;
        }
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        fs::File::open(path.parent().unwrap())?.sync_all()?;
        let obsolete = if finished(record) { &active } else { &archived };
        match fs::remove_file(obsolete) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("新记录已保存，但旧位置未移除；请核对记录"),
        }
        Ok(())
    }
    fn archive(&self, record: &Operation) -> Result<()> {
        let source = self.directory.join(format!("{}.json", record.id));
        if finished(record) && source.try_exists()? {
            let archive = self.directory.join("archive");
            super::super::private_directory(&archive)?;
            fs::rename(source, archive.join(format!("{}.json", record.id)))?;
            fs::File::open(&archive)?.sync_all()?;
            fs::File::open(&self.directory)?.sync_all()?;
        }
        Ok(())
    }
}

fn finished(record: &Operation) -> bool {
    matches!(record.status.as_str(), "succeeded" | "failed" | "resolved")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{operations::Plan, output::Output};
    use serde_json::Value;
    use std::os::unix::fs::MetadataExt;

    fn record(id: &str) -> Operation {
        Operation {
            id: id.into(),
            request: format!("request-{id}"),
            plan: Plan {
                id: format!("plan-{id}"),
                expires: 0,
                action: crate::Action::Ls { project: None },
                fingerprint: Value::Null,
                branches: vec![],
                common: None,
                prompt: None,
            },
            status: "succeeded".into(),
            stages: vec!["完成".into()],
            output: Output::default(),
            error: None,
        }
    }
    fn legacy(root: &Path, records: BTreeMap<String, Operation>) -> Vec<u8> {
        let data = serde_json::to_vec(&records).unwrap();
        fs::write(root.join("operations.json"), &data).unwrap();
        data
    }

    #[test]
    fn final_save_failure_preserves_active_blocker_and_archived_failures_are_retained() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(temp.path()).unwrap();
        let mut operation = record("active");
        operation.status = "running".into();
        store.save(&operation).unwrap();
        fs::write(store.directory.join("archive"), "blocked archive").unwrap();
        operation.status = "succeeded".into();
        assert!(store.save(&operation).is_err());
        assert_eq!(
            Store::read_active(temp.path()).unwrap()["active"].status,
            "running"
        );
        fs::remove_file(store.directory.join("archive")).unwrap();
        operation.status = "failed".into();
        store.save(&operation).unwrap();
        assert!(Store::read_active(temp.path()).unwrap().is_empty());
        assert_eq!(Store::read(temp.path()).unwrap()["active"].status, "failed");
    }

    #[test]
    fn active_queries_skip_archived_history_without_deleting_it() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(temp.path()).unwrap();
        for n in 0..32 {
            store.save(&record(&format!("done-{n}"))).unwrap();
        }
        let mut running = record("active");
        running.status = "running".into();
        store.save(&running).unwrap();
        assert_eq!(Store::read_active(temp.path()).unwrap().len(), 1);
        assert_eq!(Store::read(temp.path()).unwrap().len(), 33);
        // 模拟最终状态已落盘、尚未归档时退出；下次打开只归档，不重跑。
        let archived = store.path("done-0").unwrap();
        fs::rename(&archived, store.directory.join("done-0.json")).unwrap();
        assert_eq!(Store::open_active(temp.path()).unwrap().1.len(), 1);
        assert!(archived.exists());
        fs::write(&archived, "broken historical record").unwrap();
        assert_eq!(Store::open_active(temp.path()).unwrap().1.len(), 1);
        assert!(
            Store::read(temp.path()).is_err(),
            "历史查询仍须报告损坏，不丢弃记录"
        );
    }

    #[test]
    fn readonly_queries_never_create_or_migrate_and_prefer_published_records() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert!(Store::read(&missing).unwrap().is_empty());
        assert!(!missing.exists());
        legacy(temp.path(), BTreeMap::from([("one".into(), record("one"))]));
        assert_eq!(Store::read(temp.path()).unwrap().len(), 1);
        assert!(!temp.path().join("operations").exists());
        let (store, _) = Store::open(temp.path()).unwrap();
        fs::write(temp.path().join("operations.json"), "obsolete backup").unwrap();
        assert_eq!(Store::read(temp.path()).unwrap().len(), 1);
        fs::write(store.path("one").unwrap(), "broken current record").unwrap();
        assert!(Store::read(temp.path()).is_err());
    }

    #[test]
    fn migration_keeps_backup_and_only_updates_the_target_record() {
        let temp = tempfile::tempdir().unwrap();
        let original = legacy(
            temp.path(),
            ["one", "two"]
                .into_iter()
                .map(|id| (id.into(), record(id)))
                .collect(),
        );
        // 模拟迁移中断遗留的暂存目录，未发布内容不能覆盖原始记录。
        let interrupted = temp.path().join(".operations-interrupted");
        fs::create_dir(&interrupted).unwrap();
        fs::write(interrupted.join("one.json"), "partial").unwrap();
        let (store, mut records) = Store::open(temp.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            fs::read(temp.path().join("operations.json")).unwrap(),
            original
        );
        let other = store.path("two").unwrap();
        let other_inode = fs::metadata(&other).unwrap().ino();
        let other_bytes = fs::read(&other).unwrap();
        records.get_mut("one").unwrap().status = "resolved".into();
        store.save(&records["one"]).unwrap();
        assert_eq!(fs::metadata(&other).unwrap().ino(), other_inode);
        assert_eq!(fs::read(&other).unwrap(), other_bytes);
        // 新目录发布后旧文件只是备份，即使改变也不会重新导入或复活旧状态。
        fs::write(temp.path().join("operations.json"), "obsolete backup").unwrap();
        let (_, reopened) = Store::open(temp.path()).unwrap();
        assert_eq!(reopened["one"].status, "resolved");
        assert_eq!(reopened["two"].request, "request-two");
        assert_eq!(fs::metadata(other).unwrap().ino(), other_inode);
        assert_eq!(
            fs::metadata(&store.directory).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.path("one").unwrap()).unwrap().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn migration_failure_preserves_source_and_never_publishes_partial_history() {
        let temp = tempfile::tempdir().unwrap();
        let original = legacy(
            temp.path(),
            BTreeMap::from([
                ("a-valid".into(), record("a-valid")),
                ("z-invalid".into(), record("../escape")),
            ]),
        );
        assert!(Store::open(temp.path()).is_err());
        assert!(!temp.path().join("operations").exists());
        assert_eq!(
            fs::read(temp.path().join("operations.json")).unwrap(),
            original
        );
        legacy(
            temp.path(),
            BTreeMap::from([("a-valid".into(), record("a-valid"))]),
        );
        let (_, records) = Store::open(temp.path()).unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn invalid_new_records_stop_loading_without_falling_back_to_legacy() {
        let temp = tempfile::tempdir().unwrap();
        legacy(temp.path(), BTreeMap::from([("one".into(), record("one"))]));
        let (store, _) = Store::open(temp.path()).unwrap();
        let path = store.path("one").unwrap();
        fs::write(&path, "partial JSON").unwrap();
        assert!(Store::open(temp.path()).is_err());
        store.save(&record("two")).unwrap();
        fs::write(&path, serde_json::to_vec(&record("two")).unwrap()).unwrap();
        assert!(Store::open(temp.path()).is_err());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(store.path("two").unwrap(), &path).unwrap();
        assert!(Store::open(temp.path()).is_err());
        assert!(store.save(&record("../escape")).is_err());
        assert!(store.save(&record("")).is_err());
    }
}
