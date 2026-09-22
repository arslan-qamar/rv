use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{bail, Context, Result};
use argon2::Argon2;
use rand::{rngs::OsRng, RngCore};
use std::{
    fs,
    path::{Path, PathBuf},
};

const HEADER: &[u8] = b"RVSHOT1";
const INTERVAL_US: u64 = 60_000_000;

pub fn directory() -> PathBuf {
    super::config_path().with_file_name("screenshots")
}

pub fn derive_key(password: &[u8], salt: &[u8; 16]) -> Result<[u8; 32]> {
    let mut key = [0; 32];
    Argon2::default()
        .hash_password_into(password, salt, &mut key)
        .map_err(|e| anyhow::anyhow!("derive screenshot key: {e}"))?;
    Ok(key)
}

pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0; 12];
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(key).unwrap();
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow::anyhow!("encrypt screenshot"))?;
    let mut result = Vec::with_capacity(HEADER.len() + nonce.len() + ciphertext.len());
    result.extend_from_slice(HEADER);
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

pub fn decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < HEADER.len() + 12 + 16 || !data.starts_with(HEADER) {
        bail!("invalid screenshot file")
    }
    Aes256Gcm::new_from_slice(key)
        .unwrap()
        .decrypt(
            Nonce::from_slice(&data[HEADER.len()..HEADER.len() + 12]),
            &data[HEADER.len() + 12..],
        )
        .map_err(|_| anyhow::anyhow!("screenshot authentication failed"))
}

fn entries(dir: &Path) -> Result<Vec<(u64, PathBuf, u64)>> {
    let mut result = Vec::new();
    if !dir.exists() {
        return Ok(result);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "shot") {
            if let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse().ok())
            {
                result.push((id, path, entry.metadata()?.len()));
            }
        }
    }
    result.sort_by_key(|entry| entry.0);
    Ok(result)
}

pub fn list() -> Result<Vec<u64>> {
    Ok(entries(&directory())?
        .into_iter()
        .rev()
        .take(1000)
        .map(|e| e.0)
        .collect())
}

pub fn get(id: u64, key: &[u8; 32]) -> Result<Vec<u8>> {
    let data = fs::read(directory().join(format!("{id}.shot")))?;
    decrypt(key, &data)
}

pub fn store(id: u64, frame: &[u8], key: &[u8; 32]) -> Result<()> {
    let dir = directory();
    fs::create_dir_all(&dir)?;
    let mut existing = entries(&dir)?;
    if existing
        .last()
        .is_some_and(|e| id.saturating_sub(e.0) < INTERVAL_US)
    {
        return Ok(());
    }
    let data = encrypt(key, frame)?;
    let capacity = volume_capacity(&dir)?;
    let limit = capacity / 20;
    if data.len() as u64 > limit {
        bail!("screenshot exceeds storage quota")
    }
    let mut used: u64 = existing.iter().map(|e| e.2).sum();
    while used.saturating_add(data.len() as u64) > limit {
        let (_, path, size) = existing.remove(0);
        fs::remove_file(path)?;
        used = used.saturating_sub(size);
    }
    let final_path = dir.join(format!("{id}.shot"));
    let temporary = dir.join(format!("{id}.tmp"));
    fs::write(&temporary, data).context("write encrypted screenshot")?;
    fs::rename(temporary, final_path)?;
    Ok(())
}

#[cfg(windows)]
fn volume_capacity(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut total = 0;
    let mut available = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            &mut total,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(total)
}

#[cfg(not(windows))]
fn volume_capacity(_: &Path) -> Result<u64> {
    Ok(1_000_000_000)
}

#[cfg(windows)]
pub fn protect_key(key: &[u8; 32]) -> Result<Vec<u8>> {
    dpapi(key, true)
}
#[cfg(windows)]
pub fn unprotect_key(data: &[u8]) -> Result<[u8; 32]> {
    let raw = dpapi(data, false)?;
    Ok(raw
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid protected key"))?)
}

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    use std::ffi::c_void;
    #[repr(C)]
    struct Blob {
        len: u32,
        data: *mut u8,
    }
    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            input: *mut Blob,
            description: *const u16,
            entropy: *mut Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
        fn CryptUnprotectData(
            input: *mut Blob,
            description: *mut *mut u16,
            entropy: *mut Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            output: *mut Blob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(ptr: *mut c_void) -> *mut c_void;
    }
    let mut input = Blob {
        len: data.len() as u32,
        data: data.as_ptr() as *mut u8,
    };
    let mut output = Blob {
        len: 0,
        data: std::ptr::null_mut(),
    };
    // LOCAL_MACHINE lets the service recover the password-derived key after reboot.
    // File ACLs limit access to the protected key to SYSTEM and administrators.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &mut input,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0x4,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &mut input,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let result = unsafe { std::slice::from_raw_parts(output.data, output.len as usize).to_vec() };
    unsafe {
        LocalFree(output.data.cast());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encrypted_files_roundtrip_and_detect_tampering() {
        let key = derive_key(b"sample-password", b"unique-salt-1234").unwrap();
        let mut sealed = encrypt(&key, b"jpeg data").unwrap();
        assert_ne!(sealed, b"jpeg data");
        assert_eq!(decrypt(&key, &sealed).unwrap(), b"jpeg data");
        *sealed.last_mut().unwrap() ^= 1;
        assert!(decrypt(&key, &sealed).is_err());
    }
}
