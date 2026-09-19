use anyhow::{bail, Context, Result};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand::{rngs::OsRng, RngCore};
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

const NAME: &str = "RemoteViewerHost";
type Latest = Arc<(Mutex<Option<Vec<u8>>>, Condvar)>;

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
    let latest: Latest = Arc::new((Mutex::new(None), Condvar::new()));
    let viewer_busy = Arc::new(AtomicBool::new(false));
    let ipc = TcpListener::bind(("127.0.0.1", config.ipc_port))?;
    let lan = TcpListener::bind(("0.0.0.0", config.port))?;
    ipc.set_nonblocking(true)?;
    lan.set_nonblocking(true)?;
    handler.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP))?;
    while !stopped.load(Ordering::SeqCst) {
        match ipc.accept() {
            Ok((stream, _)) => {
                let c = config.clone();
                let l = latest.clone();
                thread::spawn(move || {
                    if let Err(e) = agent_session(stream, &c, &l) {
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
                    thread::spawn(move || {
                        if let Err(e) = viewer_session(stream, &c, &hash, &l) {
                            log_error(&format!("viewer: {e:#}"));
                        }
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

fn agent_session(mut stream: TcpStream, config: &Config, latest: &Latest) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (kind, token) = read_message(&mut stream, 128)?;
    if kind != AUTH || token != config.agent_token.as_bytes() {
        bail!("bad agent token")
    }
    write_message(&mut stream, AUTH_SUCCESS, &[])?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
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
        *lock.lock().unwrap() = Some(payload);
        changed.notify_all();
    }
}

fn viewer_session(
    mut stream: TcpStream,
    _config: &Config,
    password_hash: &str,
    latest: &Latest,
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
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let disconnected = Arc::new(AtomicBool::new(false));
    let reader_flag = disconnected.clone();
    let mut reader = stream.try_clone()?;
    thread::spawn(move || {
        loop {
            match read_message(&mut reader, 32) {
                Ok((PONG, _)) => (),
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
            write_message(&mut stream, PING, &[])?;
            continue;
        };
        let id = u64::from_be_bytes(frame[..8].try_into()?);
        if id == last_id {
            write_message(&mut stream, PING, &[])?;
            continue;
        }
        let width = u32::from_be_bytes(frame[8..12].try_into()?);
        let height = u32::from_be_bytes(frame[12..16].try_into()?);
        if last_size != (width, height) {
            let mut size = Vec::with_capacity(8);
            size.extend_from_slice(&width.to_be_bytes());
            size.extend_from_slice(&height.to_be_bytes());
            write_message(&mut stream, SCREEN_INFO, &size)?;
            last_size = (width, height);
        }
        write_message(&mut stream, FRAME, &frame)?;
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
