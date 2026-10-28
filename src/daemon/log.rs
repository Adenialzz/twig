//! 诊断日志有界保留；可靠投递记录不参与日志轮转。
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
static FILE: OnceLock<Mutex<PathBuf>> = OnceLock::new();
pub fn init(path: PathBuf) {
    let _ = FILE.set(Mutex::new(path));
}
pub fn error(message: &str) {
    let Some(path) = FILE.get() else {
        eprintln!("{message}");
        return;
    };
    let path = path.lock().unwrap();
    let result = (|| -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        if fs::metadata(&*path).is_ok_and(|m| m.len() >= 1024 * 1024) {
            fs::rename(&*path, path.with_extension("log.1"))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&*path)?;
        writeln!(
            file,
            "{} {message}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
    })();
    if let Err(error) = result {
        eprintln!("无法写入后台诊断日志：{error}");
    }
}
