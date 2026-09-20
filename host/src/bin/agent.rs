#![windows_subsystem = "windows"]

use anyhow::{bail, Result};
use image::{codecs::jpeg::JpegEncoder, ColorType};
use remote_viewer_host::*;
use std::{
    net::{Shutdown, TcpStream},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_capture::{
    dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat, Error as CaptureError},
    monitor::Monitor,
};

struct ControlState {
    active: bool,
    connected: bool,
}
type Control = Arc<(Mutex<ControlState>, Condvar)>;

fn capture(stream: &mut TcpStream, quality: u8, fps: u8, control: &Control) -> Result<()> {
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut previous = Instant::now() - interval;
    let mut duplication = None;
    let mut packed = Vec::new();
    let mut previous_bgra = Vec::new();
    let mut rgb = Vec::new();
    loop {
        {
            let (lock, changed) = &**control;
            let state = lock.lock().unwrap();
            let state = changed
                .wait_while(state, |state| state.connected && !state.active)
                .unwrap();
            if !state.connected {
                bail!("service disconnected")
            }
        }
        if duplication.is_none() {
            let monitor = Monitor::primary()?;
            duplication = Some(DxgiDuplicationApi::new_options(
                monitor,
                &[DxgiDuplicationFormat::Bgra8],
            )?);
            previous_bgra.clear();
            previous = Instant::now() - interval;
        }
        let is_active = || control.0.lock().unwrap().active;
        let mut frame = match duplication.as_mut().unwrap().acquire_next_frame(33) {
            Ok(frame) => frame,
            Err(CaptureError::Timeout) => continue,
            Err(CaptureError::AccessLost) => {
                duplication = None;
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if !is_active() {
            drop(frame);
            duplication = None;
            previous_bgra.clear();
            continue;
        }
        let remaining = interval.saturating_sub(previous.elapsed());
        if !remaining.is_zero() {
            drop(frame);
            thread::sleep(remaining);
            continue;
        }
        if !is_active() {
            drop(frame);
            duplication = None;
            previous_bgra.clear();
            continue;
        }
        let buffer = frame.buffer()?;
        if buffer.format() != DxgiDuplicationFormat::Bgra8 {
            bail!("unsupported capture pixel format")
        }
        let (width, height) = (buffer.width(), buffer.height());
        let source = buffer.as_nopadding_buffer(&mut packed);
        if previous_bgra.as_slice() == source {
            previous = Instant::now();
            continue;
        }
        previous_bgra.clear();
        previous_bgra.extend_from_slice(source);
        rgb.resize(width as usize * height as usize * 3, 0);
        for (src, dst) in source.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
        }
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality).encode(
            &rgb,
            width,
            height,
            ColorType::Rgb8.into(),
        )?;
        let id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
        write_message(stream, FRAME, &frame_payload(id, width, height, &jpeg))?;
        previous = Instant::now();
    }
}

fn connect_and_capture() -> Result<()> {
    let config = load_config()?;
    let mut stream = TcpStream::connect(("127.0.0.1", config.ipc_port))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    write_message(&mut stream, AUTH, config.agent_token.as_bytes())?;
    if read_message(&mut stream, 32)?.0 != AUTH_SUCCESS {
        bail!("agent authentication rejected")
    }
    stream.set_read_timeout(None)?;
    let control: Control = Arc::new((
        Mutex::new(ControlState {
            active: false,
            connected: true,
        }),
        Condvar::new(),
    ));
    let reader_control = control.clone();
    let mut reader = stream.try_clone()?;
    let reader_thread = thread::spawn(move || {
        loop {
            match read_message(&mut reader, 32) {
                Ok((AGENT_START, _)) => {
                    let mut state = reader_control.0.lock().unwrap();
                    state.active = true;
                    reader_control.1.notify_all();
                }
                Ok((AGENT_STOP, _)) => {
                    let mut state = reader_control.0.lock().unwrap();
                    state.active = false;
                    reader_control.1.notify_all();
                }
                _ => break,
            }
        }
        let mut state = reader_control.0.lock().unwrap();
        state.connected = false;
        state.active = false;
        reader_control.1.notify_all();
    });
    let result = capture(&mut stream, config.jpeg_quality, config.max_fps, &control);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader_thread.join();
    result
}

fn main() {
    loop {
        if let Err(error) = connect_and_capture() {
            let path = std::env::var("LOCALAPPDATA")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir())
                .join("RVHost")
                .join("agent.log");
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, format!("{error:#}\n"));
        }
        thread::sleep(Duration::from_secs(2));
    }
}
