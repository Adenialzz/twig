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
pub(super) struct Store {
    pub(super) directory: PathBuf,
}
impl Store {
    pub(super) fn open(root: &Path) -> Result<(Self, BTreeMap<String, Operation>)> {
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
        super::super::private_directory(&directory)?;
        let store = Self { directory };
        let mut records = BTreeMap::new();
        for entry in fs::read_dir(&store.directory)? {
            let entry = entry?;
            let path = entry.path();
            // NamedTempFile 在崩溃时可能留下未发布的临时文件。
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            ensure!(
                entry.file_type()?.is_file(),
                "操作记录不是普通文件：{}",
                path.display()
            );
            let record: Operation = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("操作记录损坏，保留文件并停止启动：{}", path.display()))?;
            ensure!(
                store.path(&record.id)? == path,
                "操作记录文件名与 ID 不一致：{}",
                path.display()
            );
            records.insert(record.id.clone(), record);
        }
        Ok((store, records))
    }
    pub(super) fn path(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
            "操作 ID 不能用作记录文件名：{id}"
        );
        Ok(self.directory.join(format!("{id}.json")))
    }
    pub(super) fn save(&self, record: &Operation) -> Result<()> {
        let path = self.path(&record.id)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        {
            let mut writer = BufWriter::new(temp.as_file_mut());
            serde_json::to_writer(&mut writer, record)?;
            writer.flush()?;
        }
        temp.as_file().sync_all()?;
        temp.persist(path)?;
        Ok(())
    }
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
