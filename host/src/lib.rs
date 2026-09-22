use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;

pub const VERSION: u8 = 1;
pub const AUTH: u8 = 1;
pub const AUTH_SUCCESS: u8 = 2;
pub const AUTH_FAILURE: u8 = 3;
pub const SCREEN_INFO: u8 = 4;
pub const FRAME: u8 = 5;
pub const PING: u8 = 6;
pub const PONG: u8 = 7;
pub const BUSY: u8 = 8;
pub const DISCONNECT: u8 = 9;
pub const SNAPSHOT_LIST: u8 = 10;
pub const SNAPSHOT_LIST_REPLY: u8 = 11;
pub const SNAPSHOT_GET: u8 = 12;
pub const SNAPSHOT_FRAME: u8 = 13;
pub const SNAPSHOT_ERROR: u8 = 14;
// Local IPC only. These values are never accepted from or sent to LAN viewers.
pub const AGENT_START: u8 = 100;
pub const AGENT_STOP: u8 = 101;
pub const AGENT_SNAPSHOT: u8 = 102;
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub device_name: String,
    pub port: u16,
    pub jpeg_quality: u8,
    pub max_fps: u8,
    pub agent_token: String,
    pub ipc_port: u16,
}

pub fn config_path() -> PathBuf {
    PathBuf::from(std::env::var("PROGRAMDATA").unwrap_or_else(|_| "C:\\ProgramData".into()))
        .join("RVHost")
        .join("config.json")
}

pub fn load_config() -> Result<Config> {
    Ok(serde_json::from_slice(&std::fs::read(config_path())?)?)
}

pub fn password_hash_path() -> PathBuf {
    config_path().with_file_name("password.hash")
}

pub mod snapshots;

pub fn write_message<W: Write>(writer: &mut W, kind: u8, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD {
        bail!("payload too large")
    }
    writer.write_all(&[VERSION, kind])?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

pub fn read_message<R: Read>(reader: &mut R, limit: usize) -> Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 6];
    reader.read_exact(&mut header)?;
    if header[0] != VERSION {
        bail!("unsupported protocol version")
    }
    let len = u32::from_be_bytes(header[2..6].try_into()?) as usize;
    if len > limit || len > MAX_PAYLOAD {
        bail!("payload limit exceeded")
    }
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    Ok((header[1], payload))
}

pub fn frame_payload(id: u64, width: u32, height: u32, jpeg: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(20 + jpeg.len());
    payload.extend_from_slice(&id.to_be_bytes());
    payload.extend_from_slice(&width.to_be_bytes());
    payload.extend_from_slice(&height.to_be_bytes());
    payload.extend_from_slice(&(jpeg.len() as u32).to_be_bytes());
    payload.extend_from_slice(jpeg);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_roundtrip_and_limit() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, FRAME, &frame_payload(3, 800, 600, &[1, 2])).unwrap();
        let (kind, payload) = read_message(&mut bytes.as_slice(), 100).unwrap();
        assert_eq!(kind, FRAME);
        assert_eq!(u64::from_be_bytes(payload[..8].try_into().unwrap()), 3);
        assert!(read_message(&mut bytes.as_slice(), 3).is_err());
    }
}
