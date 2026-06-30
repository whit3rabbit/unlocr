use std::collections::HashSet;
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
        // `df -k <path>` prints one header + one data row for the path's
        // filesystem. Parse only that first data row: bind/stacked mounts can
        // make df emit several rows, and joining them all into one token list
        // desyncs the columns from the single header, so a header-indexed
        // "Avail" value would land on the wrong column (or a non-numeric token).
        let data: Vec<&str> = lines[1].split_whitespace().collect();

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

/// Number of logical CPU cores (includes hyperthreads / SMT).
pub fn get_cpu_logical_cores() -> Option<u32> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.logicalcpu"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else if cfg!(target_os = "linux") {
        let out = Command::new("nproc").output().ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors",
            ])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        None
    }
}

/// Number of physical CPU cores (excludes hyperthreads / SMT).
pub fn get_cpu_physical_cores() -> Option<u32> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.physicalcpu"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else if cfg!(target_os = "linux") {
        // Parse /proc/cpuinfo for unique physical id + core id pairs.
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        let mut seen = HashSet::new();
        let mut phys_id: Option<String> = None;
        let mut core_id: Option<String> = None;
        for line in cpuinfo.lines() {
            if let Some(val) = line.strip_prefix("physical id\t: ") {
                phys_id = Some(val.trim().to_string());
            } else if let Some(val) = line.strip_prefix("core id\t\t: ") {
                core_id = Some(val.trim().to_string());
            }
            if phys_id.is_some() && core_id.is_some() {
                seen.insert((phys_id.take()?, core_id.take()?));
            }
        }
        let count = seen.len() as u32;
        if count > 0 {
            Some(count)
        } else {
            None
        }
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum",
            ])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        stdout.parse().ok()
    } else {
        None
    }
}

/// CPU model / brand string (e.g. "Apple M2 Pro", "Intel(R) Core(TM) i7-10700K").
pub fn get_cpu_model() -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else if cfg!(target_os = "linux") {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        for line in cpuinfo.lines() {
            if let Some(val) = line.strip_prefix("model name\t: ") {
                let v = val.trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        None
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor).Name",
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}

/// GPU description string (e.g. "Apple M2 Pro (Metal)").
pub fn detect_gpu() -> Option<String> {
    if cfg!(target_os = "macos") {
        // system_profiler SPDisplaysDataType surfaces the GPU chipset model and
        // Metal support. We take the first Chipset Model line.
        let out = Command::new("system_profiler")
            .args(["SPDisplaysDataType"])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            if let Some(val) = line.trim().strip_prefix("Chipset Model: ") {
                let device = val.trim().to_string();
                // The same block also reports "Metal: Supported" when the GPU
                // supports it; append "(Metal)" as a suffix in that case.
                let has_metal = stdout.contains("Metal: Supported");
                return if has_metal {
                    Some(format!("{device} (Metal)"))
                } else {
                    Some(device)
                };
            }
        }
        None
    } else if cfg!(target_os = "linux") {
        // Try nvidia-smi first (structured, most reliable for NVIDIA GPUs).
        let smi = Command::new("nvidia-smi")
            .args(["--query-gpu=name", "--format=csv,noheader"])
            .output();
        if let Ok(out) = smi {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                // nvidia-smi may return multiple lines (multi-GPU); take the first.
                let first = s.lines().next().unwrap_or("").trim().to_string();
                if !first.is_empty() {
                    return Some(format!("{first} (CUDA)"));
                }
            }
        }
        // Fall back to lspci for AMD / Intel / basic detection. Call lspci
        // directly and filter in Rust: avoids a `sh -c "... | grep"` subprocess
        // pair (which also depends on sh + grep being on PATH).
        let lspci = Command::new("lspci").output().ok()?;
        let stdout = String::from_utf8_lossy(&lspci.stdout);
        for line in stdout.lines() {
            let lower = line.to_ascii_lowercase();
            if !(lower.contains("vga") || lower.contains("3d") || lower.contains("display")) {
                continue;
            }
            // "XX:XX.X <class>[ [code]]: <vendor model> [(rev NN)]"
            // The third colon-separated field (after the slot and the class) is
            // the vendor + model; strip a trailing "(rev NN)" tag so the name
            // reads cleanly instead of "HD Graphics 630 (rev 04)".
            if let Some(desc) = line.split(':').nth(2) {
                let desc = match desc.rfind(" (rev") {
                    Some(i) => &desc[..i],
                    None => desc,
                };
                let gpu = desc.trim().to_string();
                if !gpu.is_empty() {
                    return Some(gpu);
                }
            }
        }
        None
    } else if cfg!(target_os = "windows") {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_VideoController).Name",
            ])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    } else {
        None
    }
}
