use std::path::{Path, PathBuf};

pub fn project_dir(repo: &Path, registry: &Path) -> PathBuf {
    let common = repo.join(".git").canonicalize().unwrap();
    let hash = common
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    registry
        .parent()
        .unwrap()
        .join("projects")
        .join(format!("{hash:016x}"))
}
