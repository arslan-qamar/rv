use anyhow::{bail, Context, Result};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::{rngs::OsRng, RngCore};
use remote_viewer_host::snapshots;
use remote_viewer_host::*;
use std::{
    fs,
    io::Write,
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::Duration,
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const NAME: &str = "RVHost";
type Latest = Arc<(Mutex<Option<Vec<u8>>>, Condvar)>;
#[derive(Clone, Copy)]
struct ViewerState {
    active: bool,
    generation: u64,
}
type ViewerActivity = Arc<(Mutex<ViewerState>, Condvar)>;

fn set_viewer_active(activity: &ViewerActivity, active: bool) {
    let (lock, changed) = &**activity;
    let mut state = lock.lock().unwrap();
    if state.active != active {
        state.active = active;
        state.generation = state.generation.wrapping_add(1);
        changed.notify_all();
    }
}

fn log_error(message: &str) {
    let path = config_path().with_file_name("host.log");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn init(path: &str) -> Result<()> {
    let input = fs::read_to_string(path)?;
    let mut lines = input.lines();
    let name = lines
        .next()
        .context("missing device name")?
        .trim()
        .to_string();
    let port: u16 = lines.next().context("missing port")?.trim().parse()?;
    let password = lines
        .next()
        .context("missing password")?
        .trim_end_matches('\r');
    if name.is_empty()
        || name.len() > 64
        || port == 0
        || port == 45901
        || password.len() < 8
        || password.len() > 256
    {
        bail!("invalid device name, port, or password (8-256 bytes)")
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| anyhow::anyhow!("password hash: {error}"))?
        .to_string();
    let old_password_matches = fs::read_to_string(password_hash_path())
        .ok()
        .is_some_and(|old| {
            PasswordHash::new(&old).ok().is_some_and(|stored| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &stored)
                    .is_ok()
            })
        });
    let key_path = config_path().with_file_name("screenshots.key");
    if !old_password_matches || !key_path.exists() {
        // Changing the login password starts a fresh archive: old files cannot
        // be decrypted with the new password-derived key.
        if snapshots::directory().exists() {
            fs::remove_dir_all(snapshots::directory())?;
        }
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let key = snapshots::derive_key(password.as_bytes(), &salt)?;
        fs::create_dir_all(config_path().parent().unwrap())?;
        fs::write(&key_path, snapshots::protect_key(&key)?)?;
    }
    let mut token = [0u8; 32];
    OsRng.fill_bytes(&mut token);
    let agent_token = token.iter().map(|b| format!("{b:02x}")).collect();
    let config = Config {
        device_name: name,
        port,
        jpeg_quality: 70,
        max_fps: 30,
        agent_token,
        ipc_port: 45901,
    };
    let target = config_path();
    fs::create_dir_all(target.parent().unwrap())?;
    fs::write(&target, serde_json::to_vec_pretty(&config)?)?;
    fs::write(password_hash_path(), hash)?;
    fs::create_dir_all(snapshots::directory())?;
    // The installer grants ordinary users read-only access so an interactive agent can read its token.
    Ok(())
}

define_windows_service!(ffi_main, service_main);

fn service_main(_: Vec<std::ffi::OsString>) {
    if let Err(error) = run_service() {
        log_error(&format!("fatal: {error:#}"));
    }
}

fn status(state: ServiceState, accepted: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

fn run_service() -> Result<()> {
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    let handler = service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop => {
            signal.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handler.set_service_status(status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
    ))?;
    let config = Arc::new(load_config()?);
    let password_hash = Arc::new(fs::read_to_string(password_hash_path())?);
    let snapshot_key = Arc::new(snapshots::unprotect_key(&fs::read(
        config_path().with_file_name("screenshots.key"),
    )?)?);
    let latest: Latest = Arc::new((Mutex::new(None), Condvar::new()));
    let viewer_activity: ViewerActivity = Arc::new((
        Mutex::new(ViewerState {
            active: false,
            generation: 0,
        }),
        Condvar::new(),
    ));
    let viewer_busy = Arc::new(AtomicBool::new(false));
    let ipc = TcpListener::bind(("127.0.0.1", config.ipc_port))?;
    let lan = TcpListener::bind(("0.0.0.0", config.port))?;
    ipc.set_nonblocking(true)?;
    lan.set_nonblocking(true)?;
    handler.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP))?;
    while !stopped.load(Ordering::SeqCst) {
        match ipc.accept() {
            Ok((stream, _)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    log_error(&format!("agent socket mode: {error}"));
                    continue;
                }
                let c = config.clone();
                let l = latest.clone();
                let activity = viewer_activity.clone();
                let key = snapshot_key.clone();
                thread::spawn(move || {
                    if let Err(e) = agent_session(stream, &c, &l, &activity, &key) {
                        log_error(&format!("agent: {e:#}"));
                    }
                    let (lock, changed) = &*l;
                    *lock.lock().unwrap() = None;
                    changed.notify_all();
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(e) => log_error(&format!("IPC accept: {e}")),
        }
        match lan.accept() {
            Ok((mut stream, _)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    log_error(&format!("viewer socket mode: {error}"));
                    continue;
                }
                let _ = stream.set_nodelay(true);
                if viewer_busy
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    let _ = write_message(&mut stream, BUSY, &[]);
                } else {
                    let c = config.clone();
                    let hash = password_hash.clone();
                    let l = latest.clone();
                    let busy = viewer_busy.clone();
                    let activity = viewer_activity.clone();
                    let key = snapshot_key.clone();
                    thread::spawn(move || {
                        if let Err(e) = viewer_session(stream, &c, &hash, &l, &activity, &key) {
                            log_error(&format!("viewer: {e:#}"));
                        }
                        set_viewer_active(&activity, false);
                        let (lock, changed) = &*l;
                        *lock.lock().unwrap() = None;
                        changed.notify_all();
                        busy.store(false, Ordering::SeqCst);
                    });
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(e) => log_error(&format!("LAN accept: {e}")),
        }
        thread::sleep(Duration::from_millis(10));
    }
    handler.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

fn agent_session(
    mut stream: TcpStream,
    config: &Config,
    latest: &Latest,
    activity: &ViewerActivity,
    key: &[u8; 32],
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (kind, token) = read_message(&mut stream, 128)?;
    if kind != AUTH || token != config.agent_token.as_bytes() {
        bail!("bad agent token")
    }
    write_message(&mut stream, AUTH_SUCCESS, &[])?;
    // DXGI can pause for more than a few seconds while a VM display is being
    // resized, suspended, locked, or reconfigured. Keep authenticated local IPC
    // blocking and let EOF/socket errors detect an agent that actually exited.
    stream.set_read_timeout(None)?;
    let alive = Arc::new(AtomicBool::new(true));
    let writer_alive = alive.clone();
    let writer_activity = activity.clone();
    let mut control_stream = stream.try_clone()?;
    let control_writer = thread::spawn(move || -> Result<()> {
        let mut seen_generation = u64::MAX;
        loop {
            let (lock, changed) = &*writer_activity;
            let state = lock.lock().unwrap();
            let (state, timeout) = changed
                .wait_timeout_while(state, Duration::from_secs(60), |state| {
                    writer_alive.load(Ordering::SeqCst) && state.generation == seen_generation
                })
                .unwrap();
            if !writer_alive.load(Ordering::SeqCst) {
                return Ok(());
            }
            let kind = if timeout.timed_out() && !state.active {
                AGENT_SNAPSHOT
            } else if state.active {
                AGENT_START
            } else {
                AGENT_STOP
            };
            seen_generation = state.generation;
            drop(state);
            write_message(&mut control_stream, kind, &[])?;
        }
    });
    let result = (|| -> Result<()> {
        let mut last_snapshot = snapshots::list()?.first().copied().unwrap_or(0);
        loop {
            let (kind, payload) = read_message(&mut stream, MAX_PAYLOAD)?;
            if kind == PING {
                continue;
            }
            if kind != FRAME || payload.len() < 20 {
                bail!("bad agent frame")
            }
            let width = u32::from_be_bytes(payload[8..12].try_into()?);
            let height = u32::from_be_bytes(payload[12..16].try_into()?);
            let jpeg_len = u32::from_be_bytes(payload[16..20].try_into()?) as usize;
            if width == 0
                || height == 0
                || width > 16384
                || height > 16384
                || jpeg_len != payload.len() - 20
            {
                bail!("invalid frame dimensions")
            }
            let (lock, changed) = &**latest;
            let id = u64::from_be_bytes(payload[..8].try_into()?);
            if id.saturating_sub(last_snapshot) >= 60_000_000 {
                match snapshots::store(id, &payload, key) {
                    Ok(()) => last_snapshot = id,
                    Err(e) => log_error(&format!("snapshot store: {e:#}")),
                }
            }
            if activity.0.lock().unwrap().active {
                *lock.lock().unwrap() = Some(payload);
                changed.notify_all();
            }
        }
    })();
    alive.store(false, Ordering::SeqCst);
    activity.1.notify_all();
    let _ = control_writer.join();
    result
}

fn viewer_session(
    mut stream: TcpStream,
    _config: &Config,
    password_hash: &str,
    latest: &Latest,
    activity: &ViewerActivity,
    key: &[u8; 32],
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (kind, supplied) = read_message(&mut stream, 256)?;
    let valid = kind == AUTH
        && PasswordHash::new(password_hash).ok().is_some_and(|stored| {
            Argon2::default()
                .verify_password(&supplied, &stored)
                .is_ok()
        });
    if !valid {
        write_message(&mut stream, AUTH_FAILURE, &[])?;
        return Ok(());
    }
    write_message(&mut stream, AUTH_SUCCESS, &[])?;
    set_viewer_active(activity, true);
    // Authentication has completed. Keep the read side blocking while the writer
    // waits for desktop frames; an inherited authentication timeout would
    // otherwise disconnect an idle viewer after ten seconds.
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let disconnected = Arc::new(AtomicBool::new(false));
    let reader_flag = disconnected.clone();
    let mut reader = stream.try_clone()?;
    let reader_writer = writer.clone();
    let reader_key = *key;
    thread::spawn(move || {
        loop {
            match read_message(&mut reader, 32) {
                Ok((PONG, _)) => (),
                Ok((SNAPSHOT_LIST, payload)) if payload.is_empty() => {
                    let response = snapshots::list().map(|ids| {
                        ids.into_iter()
                            .flat_map(u64::to_be_bytes)
                            .collect::<Vec<_>>()
                    });
                    let mut out = reader_writer.lock().unwrap();
                    match response {
                        Ok(data) => {
                            let _ = write_message(&mut *out, SNAPSHOT_LIST_REPLY, &data);
                        }
                        Err(_) => {
                            let _ = write_message(&mut *out, SNAPSHOT_ERROR, &[]);
                        }
                    }
                }
                Ok((SNAPSHOT_GET, payload)) if payload.len() == 8 => {
                    let id = u64::from_be_bytes(payload.try_into().unwrap());
                    let response = snapshots::get(id, &reader_key);
                    let mut out = reader_writer.lock().unwrap();
                    match response {
                        Ok(data) => {
                            let _ = write_message(&mut *out, SNAPSHOT_FRAME, &data);
                        }
                        Err(_) => {
                            let _ = write_message(&mut *out, SNAPSHOT_ERROR, &[]);
                        }
                    }
                }
                Ok((DISCONNECT, _)) | Err(_) => break,
                _ => break,
            }
        }
        reader_flag.store(true, Ordering::SeqCst);
    });
    let mut last_id = 0u64;
    let mut last_size = (0u32, 0u32);
    loop {
        if disconnected.load(Ordering::SeqCst) {
            return Ok(());
        }
        let frame = {
            let (lock, changed) = &**latest;
            let guard = lock.lock().unwrap();
            let (guard, _) = changed
                .wait_timeout_while(guard, Duration::from_secs(1), |value| {
                    value.as_ref().is_none_or(|data| {
                        u64::from_be_bytes(data[..8].try_into().unwrap()) == last_id
                    })
                })
                .unwrap();
            guard.clone()
        };
        let Some(frame) = frame else {
            write_message(&mut *writer.lock().unwrap(), PING, &[])?;
            continue;
        };
        let id = u64::from_be_bytes(frame[..8].try_into()?);
        if id == last_id {
            write_message(&mut *writer.lock().unwrap(), PING, &[])?;
            continue;
        }
        let width = u32::from_be_bytes(frame[8..12].try_into()?);
        let height = u32::from_be_bytes(frame[12..16].try_into()?);
        if last_size != (width, height) {
            let mut size = Vec::with_capacity(8);
            size.extend_from_slice(&width.to_be_bytes());
            size.extend_from_slice(&height.to_be_bytes());
            write_message(&mut *writer.lock().unwrap(), SCREEN_INFO, &size)?;
            last_size = (width, height);
        }
        write_message(&mut *writer.lock().unwrap(), FRAME, &frame)?;
        last_id = id;
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--init-file" {
        return init(&args[2]);
    }
    service_dispatcher::start(NAME, ffi_main).context("start Windows Service dispatcher")
}
