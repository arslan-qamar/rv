#![windows_subsystem = "windows"]

use anyhow::{bail, Result};
use image::{codecs::jpeg::JpegEncoder, ColorType};
use remote_viewer_host::*;
use std::{
    net::TcpStream,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_capture::{
    dxgi_duplication_api::{DxgiDuplicationApi, DxgiDuplicationFormat, Error as CaptureError},
    monitor::Monitor,
};

fn capture(mut stream: TcpStream, quality: u8, fps: u8) -> Result<()> {
    let monitor = Monitor::primary()?;
    let mut duplication =
        DxgiDuplicationApi::new_options(monitor, &[DxgiDuplicationFormat::Bgra8])?;
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut previous = Instant::now() - interval;
    let mut heartbeat = Instant::now();
    let mut packed = Vec::new();
    let mut rgb = Vec::new();
    loop {
        let mut frame = match duplication.acquire_next_frame(33) {
            Ok(frame) => frame,
            Err(CaptureError::Timeout) => {
                if heartbeat.elapsed() >= Duration::from_secs(2) {
                    write_message(&mut stream, PING, &[])?;
                    heartbeat = Instant::now();
                }
                continue;
            }
            Err(CaptureError::AccessLost) => bail!("desktop capture access lost"),
            Err(error) => return Err(error.into()),
        };
        if previous.elapsed() < interval {
            continue;
        }
        let buffer = frame.buffer()?;
        if buffer.format() != DxgiDuplicationFormat::Bgra8 {
            bail!("unsupported capture pixel format")
        }
        let (width, height) = (buffer.width(), buffer.height());
        let source = buffer.as_nopadding_buffer(&mut packed);
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
        write_message(&mut stream, FRAME, &frame_payload(id, width, height, &jpeg))?;
        previous = Instant::now();
        heartbeat = Instant::now();
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
    capture(stream, config.jpeg_quality, config.max_fps)
}

fn main() {
    loop {
        if let Err(error) = connect_and_capture() {
            let path = config_path().with_file_name("agent.log");
            let _ = std::fs::write(path, format!("{error:#}\n"));
        }
        thread::sleep(Duration::from_secs(2));
    }
}
