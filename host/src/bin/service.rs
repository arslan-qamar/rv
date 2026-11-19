use anyhow::{bail, Context, Result};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use bytes::Bytes;
use mdns_sd::{ServiceDaemon, ServiceInfo};
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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use webrtc::{
    api::{media_engine::MediaEngine, setting_engine::SettingEngine, APIBuilder},
    data_channel::RTCDataChannel,
    peer_connection::{
        configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
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
const DISCOVERY_TYPE: &str = "_rvhost._tcp.local.";
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
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(
            file,
            "{}.{:03} {message}",
            timestamp.as_secs(),
            timestamp.subsec_millis()
        );
    }
}

fn dns_hostname() -> String {
    let computer_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| NAME.into());
    let mut label = String::with_capacity(computer_name.len());
    for character in computer_name.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_alphanumeric() {
            label.push(character);
        } else if !label.ends_with('-') {
            label.push('-');
        }
        if label.len() == 63 {
            break;
        }
    }
    let label = label.trim_matches('-');
    format!("{}.local.", if label.is_empty() { "rvhost" } else { label })
}

fn advertise(config: &Config) -> Result<(ServiceDaemon, String)> {
    let daemon = ServiceDaemon::new()?;
    let properties = [("protocol", VERSION.to_string())];
    let mut name_end = config.device_name.len().min(63);
    while !config.device_name.is_char_boundary(name_end) {
        name_end -= 1;
    }
    let info = ServiceInfo::new(
        DISCOVERY_TYPE,
        &config.device_name[..name_end],
        &dns_hostname(),
        "",
        config.port,
        &properties[..],
    )?
    .enable_addr_auto();
    let fullname = info.get_fullname().to_owned();
    daemon.register(info)?;
    Ok((daemon, fullname))
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
    let discovery = match advertise(&config) {
        Ok(discovery) => Some(discovery),
        Err(error) => {
            log_error(&format!("discovery unavailable: {error:#}"));
            None
        }
    };
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
    if let Some((daemon, fullname)) = discovery {
        if let Err(error) = daemon.unregister(&fullname) {
            log_error(&format!("discovery unregister: {error}"));
        }
        if let Err(error) = daemon.shutdown() {
            log_error(&format!("discovery shutdown: {error}"));
        }
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
    let webrtc = kind == AUTH_WEBRTC;
    let valid = (kind == AUTH || webrtc)
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
    if webrtc {
        return webrtc_viewer_session(stream, latest, key);
    }
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
                Ok((DISCONNECT, _)) => {
                    log_error("WebRTC viewer sent a disconnect message");
                    break;
                }
                Err(error) => {
                    log_error(&format!("WebRTC control TCP read ended: {error}"));
                    break;
                }
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

fn webrtc_viewer_session(mut stream: TcpStream, latest: &Latest, key: &[u8; 32]) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let (kind, payload) = read_message(&mut stream, 1024 * 1024)?;
    if kind != WEBRTC_OFFER {
        bail!("expected WebRTC offer")
    }
    let offer: RTCSessionDescription = serde_json::from_slice(&payload)?;

    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_udp_network(webrtc::ice::udp_network::UDPNetwork::Ephemeral(
        webrtc::ice::udp_network::EphemeralUDP::new(ICE_UDP_PORT_MIN, ICE_UDP_PORT_MAX)?,
    ));
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_setting_engine(setting_engine)
        .build();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let disconnected = Arc::new(AtomicBool::new(false));
    let control_disconnected = disconnected.clone();
    let control_writer = writer.clone();
    let control_key = *key;
    let mut control_reader = stream.try_clone()?;
    thread::spawn(move || {
        loop {
            match read_message(&mut control_reader, 32) {
                Ok((PONG, _)) => (),
                Ok((SNAPSHOT_LIST, payload)) if payload.is_empty() => {
                    let response = snapshots::list().map(|ids| {
                        ids.into_iter()
                            .flat_map(u64::to_be_bytes)
                            .collect::<Vec<_>>()
                    });
                    let mut out = control_writer.lock().unwrap();
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
                    let response = snapshots::get(id, &control_key);
                    let mut out = control_writer.lock().unwrap();
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
        control_disconnected.store(true, Ordering::SeqCst);
    });

    stream.set_read_timeout(None)?;
    runtime.block_on(async {
        let peer = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
        let state_peer = peer.clone();
        peer.on_ice_connection_state_change(Box::new(move |state| {
            let state_peer = state_peer.clone();
            Box::pin(async move {
                log_error(&format!("WebRTC ICE state: {state}"));
                if matches!(
                    state,
                    webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Connected
                        | webrtc::ice_transport::ice_connection_state::RTCIceConnectionState::Completed
                ) {
                    let dtls = state_peer.sctp().transport();
                    if let Some(pair) = dtls.ice_transport().get_selected_candidate_pair().await {
                        log_error(&format!("WebRTC selected ICE pair: {pair}"));
                    }
                }
            })
        }));
        peer.on_peer_connection_state_change(Box::new(|state| {
            log_error(&format!("WebRTC peer state: {state}"));
            Box::pin(async {})
        }));
        let (channel_tx, mut channel_rx) =
            tokio::sync::mpsc::unbounded_channel::<Arc<RTCDataChannel>>();
        peer.on_data_channel(Box::new(move |channel| {
            let tx = channel_tx.clone();
            Box::pin(async move {
                if channel.label() == "rv-frame-v1" {
                    let opened_channel = channel.clone();
                    channel.on_open(Box::new(move || {
                        let _ = tx.send(opened_channel);
                        Box::pin(async {})
                    }));
                }
            })
        }));
        peer.set_remote_description(offer).await?;
        let mut gathered = peer.gathering_complete_promise().await;
        let answer = peer.create_answer(None).await?;
        peer.set_local_description(answer).await?;
        let _ = gathered.recv().await;
        let local = peer
            .local_description()
            .await
            .context("missing local WebRTC answer")?;
        write_message(
            &mut *writer.lock().unwrap(),
            WEBRTC_ANSWER,
            &serde_json::to_vec(&local)?,
        )?;
        let channel = tokio::time::timeout(Duration::from_secs(30), channel_rx.recv())
            .await
            .context("WebRTC data channel timed out")?
            .context("WebRTC data channel closed")?;

        let mut last_id = 0u64;
        let mut last_tcp_ping = Instant::now();
        while !disconnected.load(Ordering::SeqCst) {
            // Keep the signaling/control TCP flow alive while frames travel over
            // WebRTC UDP. Some firewalls expire an otherwise idle TCP mapping.
            if last_tcp_ping.elapsed() >= Duration::from_secs(10) {
                write_message(&mut *writer.lock().unwrap(), PING, &[])?;
                last_tcp_ping = Instant::now();
            }
            match peer.connection_state() {
                RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Connecting
                | RTCPeerConnectionState::New
                | RTCPeerConnectionState::Unspecified => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    bail!("WebRTC peer entered {}", peer.connection_state())
                }
                RTCPeerConnectionState::Connected => (),
            }
            let frame = { latest.0.lock().unwrap().clone() };
            if let Some(frame) = frame {
                let id = u64::from_be_bytes(frame[..8].try_into()?);
                if id != last_id {
                    let width = u32::from_be_bytes(frame[8..12].try_into()?);
                    let height = u32::from_be_bytes(frame[12..16].try_into()?);
                    let jpeg_len = u32::from_be_bytes(frame[16..20].try_into()?);
                    let chunk_size = 60 * 1024 - 28;
                    let mut sent_frame = true;
                    for (index, chunk) in frame[20..].chunks(chunk_size).enumerate() {
                        // Back-pressure between chunks so a JPEG is never abandoned partway
                        // through. The single latest-frame slot replaces stale captures.
                        while channel.buffered_amount().await > 120_000
                            && !disconnected.load(Ordering::SeqCst)
                            && peer.connection_state() == RTCPeerConnectionState::Connected
                        {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        if disconnected.load(Ordering::SeqCst) {
                            sent_frame = false;
                            break;
                        }
                        if peer.connection_state() != RTCPeerConnectionState::Connected {
                            sent_frame = false;
                            break;
                        }
                        let mut packet = Vec::with_capacity(28 + chunk.len());
                        packet.extend_from_slice(b"RVF1");
                        packet.extend_from_slice(&id.to_be_bytes());
                        packet.extend_from_slice(&width.to_be_bytes());
                        packet.extend_from_slice(&height.to_be_bytes());
                        packet.extend_from_slice(&jpeg_len.to_be_bytes());
                        packet.extend_from_slice(&((index * chunk_size) as u32).to_be_bytes());
                        packet.extend_from_slice(chunk);
                        if let Err(error) = channel.send(&Bytes::from(packet)).await {
                            let state = peer.connection_state();
                            log_error(&format!(
                                "WebRTC data channel send failed at frame {id}, chunk {index}, peer state {state}: {error}"
                            ));
                            if matches!(
                                state,
                                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                            ) {
                                bail!("WebRTC data channel send failed in state {state}: {error}");
                            }
                            sent_frame = false;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            break;
                        }
                    }
                    if sent_frame {
                        last_id = id;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = peer.close().await;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--init-file" {
        return init(&args[2]);
    }
    service_dispatcher::start(NAME, ffi_main).context("start Windows Service dispatcher")
}
