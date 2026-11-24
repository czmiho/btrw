use crate::fs_safe::find_trusted_binary;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotUsage {
    pub total_bytes: u64,
    pub exclusive_bytes: u64,
}

/// Parses raw output of `btrfs filesystem du --raw -s`.
pub fn parse_du_output(output: &str) -> Result<SnapshotUsage, String> {
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 4
            && let (Ok(total), Ok(exclusive), Ok(_set_shared)) = (
                fields[0].parse::<u64>(),
                fields[1].parse::<u64>(),
                fields[2].parse::<u64>(),
            )
        {
            return Ok(SnapshotUsage {
                total_bytes: total,
                exclusive_bytes: exclusive,
            });
        }
    }
    Err("unrecognized btrfs filesystem du output".to_string())
}

/// Computes `Total` and `Exclusive` data extent usage for a snapshot via
/// `/usr/bin/btrfs filesystem du --raw -s`. Supports cooperative cancellation via `cancel`.
pub fn compute_snapshot_usage(path: &Path, cancel: &AtomicBool) -> Result<SnapshotUsage, String> {
    let btrfs_bin =
        find_trusted_binary("btrfs").ok_or_else(|| "btrfs binary not found in PATH".to_string())?;
    let mut child = Command::new(btrfs_bin)
        .args(["filesystem", "du", "--raw", "-s", "--"])
        .arg(path)
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("btrfs du spawn failed: {e}"))?;

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "btrfs du stdout unavailable".to_string())?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "btrfs du stderr unavailable".to_string())?;

    // Drain stdout and stderr concurrently to avoid pipe-buffer deadlocks.
    let stdout_handle = thread::spawn(move || {
        let mut buf = String::new();
        let mut r = stdout_pipe;
        let _ = r.read_to_string(&mut buf);
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = String::new();
        let mut r = stderr_pipe;
        let _ = r.read_to_string(&mut buf);
        buf
    });

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            return Err("cancelled".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_handle.join().unwrap_or_default();
                let stderr = stderr_handle.join().unwrap_or_default();
                if !status.success() {
                    let msg = stderr.trim();
                    return Err(if msg.is_empty() {
                        format!("btrfs du exited with {status}")
                    } else {
                        msg.to_string()
                    });
                }
                return parse_du_output(&stdout);
            }
            Ok(None) => thread::sleep(Duration::from_millis(30)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!("btrfs du wait failed: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_btrfs_du_summary() {
        let sample = "\
     Total   Exclusive  Set shared  Filename
1234567890   123456789  1111111101  /mnt/btrfs-top/@snapshots/@home.20261123T1400
";
        let usage = parse_du_output(sample).expect("parse du");
        assert_eq!(usage.total_bytes, 1_234_567_890);
        assert_eq!(usage.exclusive_bytes, 123_456_789);
    }
}
