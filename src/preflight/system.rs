use std::fs;
use std::path::Path;
use std::process::Command;

/// Retrieves the total physical RAM size of the system in bytes.
pub fn get_total_ram_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else if cfg!(target_os = "linux") {
        let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let kb: u64 = parts[1].parse().ok()?;
                    return Some(kb * 1024);
                }
            }
        }
        None
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_PhysicalMemory | Measure-Object Capacity -Sum).Sum",
            ])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        None
    }
}

/// Retrieves the available free disk space in bytes for the partition containing the given path.
pub fn get_free_disk_space_bytes(path: &Path) -> Option<u64> {
    if cfg!(target_os = "windows") {
        // Pass the path through an env var, not string interpolation, so
        // PowerShell never parses it as code. `path` is user-controlled
        // (--model-dir, or the LOCALAPPDATA/XDG_CACHE_HOME/HOME cache-dir env
        // vars); a value containing a single quote would otherwise terminate the
        // -Command string literal and inject arbitrary PowerShell. -LiteralPath
        // also stops wildcard/glob interpretation of the path.
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-Item -LiteralPath $env:UNLOCR_DISK_PATH).Volume.Free",
            ])
            .env("UNLOCR_DISK_PATH", path)
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        // macOS or Linux (Unix)
        let out = Command::new("df").arg("-k").arg(path).output().ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.len() < 2 {
            return None;
        }
        let headers: Vec<&str> = lines[0].split_whitespace().collect();
        let data_joined = lines[1..].join(" ");
        let data: Vec<&str> = data_joined.split_whitespace().collect();

        let avail_idx = headers
            .iter()
            .position(|&h| h.contains("Avail") || h.contains("Free") || h.contains("avail"));
        if let Some(idx) = avail_idx {
            if idx < data.len() {
                let kb: u64 = data[idx].parse().ok()?;
                return Some(kb * 1024);
            }
        }

        if data.len() >= 4 {
            let kb: u64 = data[3].parse().ok()?;
            return Some(kb * 1024);
        }
        None
    }
}
