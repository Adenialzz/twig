use std::path::{Path, PathBuf};

#[allow(dead_code)]
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

// 业务测试显式托管隔离后台；环境变更通过重启后台生效，不把客户端环境偷渡到 RPC。
#[allow(dead_code)]
pub mod daemon {
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        fs::File,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{Mutex, OnceLock},
        thread,
        time::{Duration, Instant},
    };
    struct Running {
        child: Child,
        environment: BTreeMap<OsString, Option<OsString>>,
    }
    static RUNNING: OnceLock<Mutex<BTreeMap<PathBuf, Running>>> = OnceLock::new();
    fn running() -> &'static Mutex<BTreeMap<PathBuf, Running>> {
        RUNNING.get_or_init(Default::default)
    }
    fn base(command: &Command, registry: &Path) -> Command {
        let mut next = Command::new(env!("CARGO_BIN_EXE_twig"));
        next.arg("--config").arg(registry);
        if let Some(cwd) = command.get_current_dir() {
            next.current_dir(cwd);
        }
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                next.env(key, value);
            } else {
                next.env_remove(key);
            }
        }
        next.env_remove("COMPLETE");
        next
    }
    pub fn ensure(command: &Command, registry: &Path) {
        let resolved = if registry.is_absolute() {
            registry.to_owned()
        } else {
            command
                .get_current_dir()
                .unwrap_or(Path::new("."))
                .join(registry)
        };
        let registry = resolved.as_path();
        let env = command
            .get_envs()
            .filter(|(key, _)| {
                let key = key.to_string_lossy();
                key == "PATH"
                    || key == "HOME"
                    || key == "CODEX_HOME"
                    || key == "SHELL"
                    || key.starts_with("TWIG_")
            })
            .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
            .collect::<BTreeMap<_, _>>();
        let mut map = running().lock().unwrap();
        if let Some(old) = map.get_mut(registry) {
            let changed = env.iter().any(|(k, v)| old.environment.get(k) != Some(v));
            if !changed && old.child.try_wait().unwrap().is_none() {
                return;
            }
            let mut old = map.remove(registry).unwrap();
            stop_child(&mut old.child);
        }
        let log = registry.with_extension("daemon-test.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let output = File::create(&log).unwrap();
        let child = base(command, registry)
            .args(["daemon", "run"])
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap();
        let mut started = Running {
            child,
            environment: env,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = base(command, registry)
                .args(["daemon", "status"])
                .output()
                .unwrap();
            if status.status.success()
                && String::from_utf8_lossy(&status.stdout).contains("instance")
            {
                break;
            }
            assert!(
                started.child.try_wait().unwrap().is_none(),
                "后台提前退出：{}",
                std::fs::read_to_string(&log).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "后台启动超时：{}",
                std::fs::read_to_string(&log).unwrap()
            );
            thread::sleep(Duration::from_millis(20));
        }
        map.insert(registry.to_owned(), started);
    }
    pub fn for_pty(command: &Command) {
        if let Some((_, Some(path))) = command.get_envs().find(|(k, _)| *k == "TWIG_TEST_CONFIG") {
            ensure(command, Path::new(path));
        }
    }
    fn stop_child(child: &mut Child) {
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    pub fn stop_under(path: &Path) {
        let mut map = running().lock().unwrap();
        let keys: Vec<_> = map
            .keys()
            .filter(|key| key.starts_with(path))
            .cloned()
            .collect();
        for key in keys {
            stop_child(&mut map.remove(&key).unwrap().child);
        }
    }
}

#[allow(dead_code)]
pub struct Daemons(pub PathBuf);
impl Drop for Daemons {
    fn drop(&mut self) {
        daemon::stop_under(&self.0);
    }
}
