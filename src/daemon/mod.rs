//! Twig 本机后台：单例、受限 Unix socket 与有界同步请求处理。
mod feishu;
mod log;
pub mod operations;
pub mod output;
mod queries;
pub(crate) mod state;
pub mod time;
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const VERSION: u32 = 1;
const MAX_FRAME: u64 = 8 * 1024 * 1024;
static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn terminate(_: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}

#[derive(Args, Clone)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Subcommand, Clone)]
pub enum Action {
    /// 前台运行 Twig 后台；供 systemd/launchd 托管
    Run,
    /// 显式后台启动并等待就绪；不启动 Codex daemon
    Start,
    /// 查询 Twig 后台健康状态；不启动服务
    Status,
    /// 有序停止 Twig 后台；保留任务和 Codex 进程
    Stop,
}

pub struct Paths {
    pub registry: PathBuf,
    pub dir: PathBuf,
    pub socket: PathBuf,
}
impl Paths {
    pub fn new(registry: &Path) -> Result<Self> {
        let absolute = crate::registry::expand(registry)?;
        let registry = canonical_location(&absolute)?;
        let id = crate::config::repository_id(&registry);
        let dir = registry
            .parent()
            .context("配置缺少父目录")?
            .join("daemon")
            .join(&id);
        // Unix socket 路径有长度限制，状态仍保存在配置旁；短路径按 UID 与配置身份隔离。
        let socket = PathBuf::from("/tmp")
            .join(format!("twig-{}", unsafe { libc::geteuid() }))
            .join(format!("{id}.sock"));
        Ok(Self {
            registry,
            dir,
            socket,
        })
    }
}

fn canonical_location(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().context("无法解析配置路径")?;
    let name = path.file_name().context("配置路径缺少文件名")?;
    Ok(canonical_location(parent)?.join(name))
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "后台运行目录必须属于当前用户且权限为 0700：{}",
        path.display()
    );
    Ok(())
}

struct Instance {
    _lock: File,
    socket: PathBuf,
}
impl Instance {
    fn acquire(paths: &Paths) -> Result<Self> {
        fs::create_dir_all(&paths.dir)?;
        fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(paths.dir.join("instance.lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "该配置的 Twig 后台已运行"
        );
        private_directory(paths.socket.parent().unwrap())?;
        if paths.socket.try_exists()? {
            fs::remove_file(&paths.socket)?;
        }
        Ok(Self {
            _lock: lock,
            socket: paths.socket.clone(),
        })
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    id: String,
    method: String,
    params: Value,
}
#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    id: String,
    result: Option<Value>,
    error: Option<String>,
}

fn read_frame<T: DeserializeOwned>(reader: &mut impl BufRead) -> Result<T> {
    let mut bytes = Vec::new();
    reader.take(MAX_FRAME + 1).read_until(b'\n', &mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_FRAME, "RPC 消息过大");
    ensure!(bytes.last() == Some(&b'\n'), "RPC 消息不完整或连接已关闭");
    Ok(serde_json::from_slice(&bytes)?)
}
fn write_frame(value: &impl Serialize, writer: &mut impl Write) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!((bytes.len() as u64) < MAX_FRAME, "RPC 消息过大");
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub fn call<T: DeserializeOwned>(registry: &Path, method: &str, params: Value) -> Result<T> {
    let paths = Paths::new(registry)?;
    let mut stream = UnixStream::connect(&paths.socket).with_context(|| {
        format!(
            "Twig 后台不可用；请执行 twig --config {} daemon start",
            paths.registry.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = Request {
        version: VERSION,
        id: crate::agent::message_id(),
        method: method.into(),
        params,
    };
    write_frame(&request, &mut stream)?;
    let response: Response = read_frame(&mut BufReader::new(stream))?;
    ensure!(
        response.version == VERSION,
        "Twig 后台协议不兼容，请统一更新并重启"
    );
    ensure!(response.id == request.id, "Twig 后台响应 ID 不匹配");
    if let Some(error) = response.error {
        bail!("{error}");
    }
    Ok(serde_json::from_value(
        response.result.unwrap_or(Value::Null),
    )?)
}

pub fn execute(registry: &Path, action: Action) -> Result<()> {
    match action {
        Action::Run => run(registry),
        Action::Start => start(registry),
        Action::Status => {
            let paths = Paths::new(registry)?;
            if !paths.socket.try_exists()? {
                println!("Twig 后台未运行");
                return Ok(());
            }
            let status: Value = call(registry, "status", Value::Null)?;
            println!("{}", serde_json::to_string_pretty(&status)?);
            Ok(())
        }
        Action::Stop => {
            let _: Value = call(registry, "stop", Value::Null)?;
            println!("Twig 后台正在有序退出；任务进程保留");
            Ok(())
        }
    }
}

fn start(registry: &Path) -> Result<()> {
    if call::<Value>(registry, "status", Value::Null).is_ok() {
        println!("Twig 后台已运行");
        return Ok(());
    }
    let paths = Paths::new(registry)?;
    fs::create_dir_all(&paths.dir)?;
    fs::set_permissions(&paths.dir, fs::Permissions::from_mode(0o700))?;
    let log_path = paths.dir.join("daemon.log");
    if fs::metadata(&log_path).is_ok_and(|m| m.len() >= 1024 * 1024) {
        fs::rename(&log_path, paths.dir.join("daemon.log.1"))?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(paths.dir.join("daemon.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--config")
        .arg(&paths.registry)
        .args(["daemon", "run"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if call::<Value>(registry, "status", Value::Null).is_ok() {
            println!("Twig 后台已就绪");
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "Twig 后台启动失败（{status}），请查看 {}",
                paths.dir.join("daemon.log").display()
            );
        }
        ensure!(
            Instant::now() < deadline,
            "后台尚未就绪；进程 {} 保留，请查看日志或 daemon status",
            child.id()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn run(registry: &Path) -> Result<()> {
    let paths = Paths::new(registry)?;
    let _instance = Instance::acquire(&paths)?;
    log::init(paths.dir.join("events.log"));
    let listener = UnixListener::bind(&paths.socket)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    unsafe {
        libc::signal(libc::SIGTERM, terminate as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, terminate as *const () as libc::sighandler_t);
    }
    let stopping = Arc::new(AtomicBool::new(false));
    let (operations, operation_worker) = operations::Service::start(&paths.registry, &paths.dir)?;
    let state = Mutex::new(state::State::start(&paths.registry)?);
    let chat = Mutex::new(None::<crate::agent::chat::Backend>);
    let agent_stop = Arc::new(AtomicBool::new(false));
    let agent_error = Arc::new(Mutex::new(None::<String>));
    let agent_worker = {
        let stop = agent_stop.clone();
        let error = agent_error.clone();
        let registry = paths.registry.clone();
        thread::spawn(move || {
            crate::console::query::install(stop.clone());
            while !stop.load(Ordering::Relaxed) {
                let result = (|| -> Result<()> {
                    let store = crate::agent::Store::new(&registry)?;
                    if !store.load()?.bindings.is_empty() {
                        store.drain()?;
                    }
                    Ok(())
                })();
                *error.lock().unwrap() = result.err().map(|e| format!("{e:#}"));
                for _ in 0..40 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        })
    };
    let feishu = feishu::Runtime::start(&paths.registry, &paths.dir)?;
    let started = Instant::now();
    let instance = crate::agent::message_id();
    let (tx, rx) = mpsc::sync_channel::<UnixStream>(32);
    let input = Arc::new(Mutex::new(rx));
    let result = thread::scope(|scope| -> Result<()> {
        for _ in 0..4 {
            let input = input.clone();
            let stopping = stopping.clone();
            let instance = &instance;
            let paths = &paths;
            let operations = &operations;
            let state = &state;
            let chat = &chat;
            let agent_error = &agent_error;
            let feishu = &feishu.health;
            scope.spawn(move || {
                crate::console::query::install(stopping.clone());
                loop {
                    let stream = input.lock().unwrap().recv();
                    let Ok(mut stream) = stream else { break; };
                    let result = (|| -> Result<()> {
                        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
                        let request: Request = read_frame(&mut BufReader::new(stream.try_clone()?))?;
                        let result = (|| -> Result<Value> {
                            ensure!(request.version == VERSION, "Twig 后台协议不兼容，请统一更新并重启");
                            match request.method.as_str() {
                                "status" => Ok(json!({"instance":instance,"version":VERSION,"pid":std::process::id(),"uptime_seconds":started.elapsed().as_secs(),"config":paths.registry,"socket":paths.socket,"stopping":stopping.load(Ordering::Relaxed),"agent_error":*agent_error.lock().unwrap(),"agent_queue":agent_queue(&paths.registry),"collection":state.lock().unwrap().health(),"operations":operations.health(),"feishu":*feishu.lock().unwrap()})),
                                "stop" => { stopping.store(true, Ordering::Relaxed); Ok(json!({"stopping":true})) },
                                method if method.starts_with("form.") => queries::handle(&paths.registry,method,request.params),
                                "details" => {
                                    let snapshot=state.lock().unwrap().snapshot();
                                    Ok(serde_json::to_value(queries::details(snapshot,serde_json::from_value(request.params)?)?)?)
                                },
                                "query.execute" => {
                                    let action:crate::Action=serde_json::from_value(request.params)?;
                                    ensure!(matches!(&action,crate::Action::Ls{..}|crate::Action::Agent(crate::agent::Cli{command:crate::agent::Action::Status|crate::agent::Action::Read{..}})),"仅接受只读业务查询");
                                    let (output,result)=output::capture(||crate::execute_business(paths.registry.clone(),action,&Value::Null));
                                    result?; Ok(serde_json::to_value(output)?)
                                },
                                "agent.chat" => chat.lock().unwrap().get_or_insert_with(||crate::agent::chat::Backend::new(paths.registry.clone())).handle(request.params),
                                "snapshot" => {
                                    let deadline=Instant::now()+Duration::from_millis(200);
                                    loop {
                                        let value=state.lock().unwrap().handle("snapshot",request.params.clone())?;
                                        if !value["snapshot"].is_null()||Instant::now()>=deadline {break Ok(value);}
                                        thread::sleep(Duration::from_millis(20));
                                    }
                                },
                                method @ ("inbox"|"viewing"|"disconnect"|"record_use") => state.lock().unwrap().handle(method,request.params),
                                method if method.starts_with("operation.") => operations.handle(method, request.params),
                                _ => bail!("未知后台方法：{}", request.method),
                            }
                        })();
                        let response = match result {
                            Ok(value) => Response { version: VERSION, id:request.id, result:Some(value), error:None },
                            Err(error) => Response { version: VERSION, id:request.id, result:None, error:Some(format!("{error:#}")) },
                        };
                        write_frame(&response, &mut stream)
                    })();
                    if let Err(error) = result { log::error(&format!("本机连接失败：{error:#}")); }
                }
            });
        }
        while !stopping.load(Ordering::Relaxed) && !TERMINATE.load(Ordering::Relaxed) {
            if let Ok(mut state) = state.try_lock() {
                state.expire();
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = tx.try_send(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e.into()),
            }
        }
        drop(tx);
        Ok(())
    });
    agent_stop.store(true, Ordering::Relaxed);
    agent_worker
        .join()
        .map_err(|_| anyhow::anyhow!("Agent 调度线程异常退出"))?;
    drop(operations);
    operation_worker
        .join()
        .map_err(|_| anyhow::anyhow!("操作执行线程异常退出"))?;
    result
}

fn agent_queue(registry: &Path) -> Value {
    match crate::agent::Store::new(registry).and_then(|store| store.load()) {
        Ok(state) => {
            let mut counts = std::collections::BTreeMap::<String, usize>::new();
            for message in state.messages {
                *counts.entry(message.status).or_default() += 1;
            }
            json!(counts)
        }
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_requires_boundary_and_valid_json() {
        assert!(read_frame::<Value>(&mut &b"{}"[..]).is_err());
        assert!(read_frame::<Value>(&mut &b"invalid\n"[..]).is_err());
        assert_eq!(read_frame::<Value>(&mut &b"{}\n"[..]).unwrap(), json!({}));
    }
    #[test]
    fn config_identity_is_stable_before_file_exists_and_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/config.toml");
        let before = Paths::new(&path).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        assert_eq!(before.socket, Paths::new(&path).unwrap().socket);
        assert_ne!(
            before.socket,
            Paths::new(&path.with_file_name("other.toml"))
                .unwrap()
                .socket
        );
    }
}
