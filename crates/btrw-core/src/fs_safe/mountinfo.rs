use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;

pub fn current_mountpoints_checked() -> io::Result<HashSet<PathBuf>> {
    use std::os::unix::ffi::OsStringExt;
    let raw = fs::read("/proc/self/mountinfo")?;
    let mut set = HashSet::new();
    for line in raw.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Some(mp_raw) = line.split(|&b| b == b' ').nth(4) else {
            continue;
        };
        let decoded = decode_mount_escapes_bytes(mp_raw);
        set.insert(PathBuf::from(std::ffi::OsString::from_vec(decoded)));
    }
    Ok(set)
}

/// Reads `/proc/self/mountinfo` and returns active mountpoints (best-effort for read-only listing).
pub fn current_mountpoints() -> HashSet<PathBuf> {
    current_mountpoints_checked().unwrap_or_default()
}

pub fn decode_mount_escapes_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let oct = &bytes[i + 1..i + 4];
            if oct.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let val: u16 = u16::from(oct[0] - b'0') * 64
                    + u16::from(oct[1] - b'0') * 8
                    + u16::from(oct[2] - b'0');
                if val <= 255 {
                    out.push(val as u8);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

pub fn decode_mount_escapes(raw: &str) -> String {
    String::from_utf8_lossy(&decode_mount_escapes_bytes(raw.as_bytes())).into_owned()
}

