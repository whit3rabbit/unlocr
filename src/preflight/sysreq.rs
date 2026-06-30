use serde::Serialize;
use std::path::Path;
use std::sync::OnceLock;

use super::system;

// ── Threshold constants ──────────────────────────────────────────────────────

/// Minimum total RAM (bytes) for a "Good" rating.
const RAM_GOOD: u64 = 8 * 1024 * 1024 * 1024; // 8 GiB
/// Minimum total RAM (bytes) for a "Marginal" rating.
const RAM_MARGINAL: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Minimum logical CPU cores for "Good".
const CPU_GOOD: u32 = 4;
/// Minimum logical CPU cores for "Marginal".
const CPU_MARGINAL: u32 = 2;

/// Minimum free disk space (bytes) on the cache partition; below is "Marginal".
const DISK_MIN: u64 = 5 * 1024 * 1024 * 1024; // 5 GiB

// ── Types ────────────────────────────────────────────────────────────────────

/// Resource rating, ordered by severity (ascending).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Good,
    Marginal,
    Insufficient,
    Unknown,
}

/// One system metric row for the frontend.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    /// Short canonical key for JS mapping (not displayed).
    pub key: String,
    /// Human-facing label, e.g. "RAM".
    pub label: String,
    /// Formatted value, e.g. "16 GiB", "8 (8 physical)", "Apple M2 Pro".
    pub value: String,
    /// Rating.
    pub status: Status,
    /// Optional recommendation hint shown when non-Good.
    pub hint: Option<String>,
}

/// Full system requirements report.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub metrics: Vec<Metric>,
    pub verdict: Status,
    pub verdict_label: String,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{bytes} B")
    }
}

fn rate_ram(bytes: Option<u64>) -> (Status, String, Option<String>) {
    match bytes {
        Some(b) if b >= RAM_GOOD => (Status::Good, fmt_bytes(b), None),
        Some(b) if b >= RAM_MARGINAL => (
            Status::Marginal,
            fmt_bytes(b),
            Some("8 GB+ recommended for Q8_0 models; 16 GB+ for BF16 or batch runs".into()),
        ),
        Some(b) => (
            Status::Insufficient,
            fmt_bytes(b),
            Some("4 GB insufficient; 8 GB+ recommended".into()),
        ),
        None => (Status::Unknown, "Unknown".into(), None),
    }
}

fn rate_cpu(logical: Option<u32>, physical: Option<u32>) -> (Status, String, Option<String>) {
    let display = match (logical, physical) {
        (Some(l), Some(p)) if l != p => format!("{l} ({p} physical)"),
        (Some(l), Some(_)) => format!("{l} cores"),
        (Some(l), None) => format!("{l} cores"),
        (None, _) => return (Status::Unknown, "Unknown".into(), None),
    };

    match logical {
        Some(c) if c >= CPU_GOOD => (Status::Good, display, None),
        Some(c) if c >= CPU_MARGINAL => (
            Status::Marginal,
            display,
            Some("4+ cores recommended".into()),
        ),
        Some(_) => (
            Status::Insufficient,
            display,
            Some("2+ cores required".into()),
        ),
        None => (Status::Unknown, display, None),
    }
}

fn rate_cpu_model(model: Option<String>) -> Metric {
    Metric {
        key: "cpu_model".into(),
        label: "CPU model".into(),
        value: model.unwrap_or_else(|| "Unknown".into()),
        status: Status::Unknown, // informational only
        hint: None,
    }
}

fn rate_gpu(gpu: Option<String>) -> Metric {
    Metric {
        key: "gpu".into(),
        label: "GPU".into(),
        value: gpu.unwrap_or_else(|| "Not detected".into()),
        status: Status::Unknown, // informational only
        hint: None,
    }
}

fn rate_disk(bytes: Option<u64>) -> (Status, String, Option<String>) {
    match bytes {
        Some(b) if b >= DISK_MIN => (Status::Good, fmt_bytes(b), None),
        Some(b) => (
            Status::Marginal,
            fmt_bytes(b),
            Some("5 GB+ free recommended for model downloads".into()),
        ),
        None => (Status::Unknown, "Unknown".into(), None),
    }
}

fn fmt_verdict_label(verdict: Status) -> &'static str {
    match verdict {
        Status::Good => "System ready",
        Status::Marginal => "Some improvements recommended",
        Status::Insufficient => "System may not meet requirements",
        Status::Unknown => "Some checks unavailable",
    }
}

/// Aggregate the worst status across a slice, ignoring Unknown.
///
/// An all-Unknown (or empty) slice means nothing was measured, so it must NOT
/// report `Good` ("System ready") for a machine whose specs are unknown; it
/// returns `Unknown` ("Some checks unavailable") instead. A single Insufficient
/// short-circuits (it is the worst possible rating).
fn worst_status(statuses: &[Status]) -> Status {
    let mut worst = Status::Good;
    let mut any_rated = false; // saw a Good/Marginal/Insufficient (a real measurement)
    for &s in statuses {
        if s == Status::Insufficient {
            return Status::Insufficient;
        }
        if s == Status::Marginal {
            worst = Status::Marginal;
            any_rated = true;
        } else if s == Status::Good {
            any_rated = true;
        }
        // Unknown carries no rating; it neither lowers `worst` nor counts as measured.
    }
    if any_rated {
        worst
    } else {
        Status::Unknown
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Hardware probes that do not change within a process lifetime. Memoized so
/// the System Requirements panel (run at startup + on every Recheck click) does
/// not re-shell out each time: `detect_gpu` spawns `system_profiler`
/// (~1-2s on macOS) and `lspci`/`nvidia-smi` on Linux, and RAM/CPU/model are
/// fixed for the session. Free disk space is NOT here (it changes as models
/// download) and is re-probed on every call.
struct StaticProbes {
    ram: Option<u64>,
    cpu_logical: Option<u32>,
    cpu_physical: Option<u32>,
    cpu_model: Option<String>,
    gpu: Option<String>,
}

static STATIC_PROBES: OnceLock<StaticProbes> = OnceLock::new();

/// Probe the static hardware specs once per process and cache the result.
fn static_probes() -> &'static StaticProbes {
    STATIC_PROBES.get_or_init(|| StaticProbes {
        ram: system::get_total_ram_bytes(),
        cpu_logical: system::get_cpu_logical_cores(),
        cpu_physical: system::get_cpu_physical_cores(),
        cpu_model: system::get_cpu_model(),
        gpu: system::detect_gpu(),
    })
}

/// Probe hardware and rate every metric against the application's known
/// thresholds. Returns a report the frontend can render directly.
pub fn check_system_requirements(cache_dir: &Path) -> SystemInfo {
    // Static specs are memoized (see StaticProbes); disk free is re-probed live.
    let p = static_probes();
    let disk_bytes = system::get_free_disk_space_bytes(cache_dir);

    let (ram_status, ram_value, ram_hint) = rate_ram(p.ram);
    let (cpu_status, cpu_value, cpu_hint) = rate_cpu(p.cpu_logical, p.cpu_physical);
    let cpu_model_metric = rate_cpu_model(p.cpu_model.clone());
    let gpu_metric = rate_gpu(p.gpu.clone());
    let (disk_status, disk_value, disk_hint) = rate_disk(disk_bytes);

    let verdict_statuses = [ram_status, cpu_status, disk_status];
    let verdict = worst_status(&verdict_statuses);

    let metrics = vec![
        Metric {
            key: "ram_total".into(),
            label: "RAM".into(),
            value: ram_value,
            status: ram_status,
            hint: ram_hint,
        },
        Metric {
            key: "cpu_cores".into(),
            label: "CPU cores".into(),
            value: cpu_value,
            status: cpu_status,
            hint: cpu_hint,
        },
        cpu_model_metric,
        gpu_metric,
        Metric {
            key: "disk_free".into(),
            label: "Free disk space".into(),
            value: disk_value,
            status: disk_status,
            hint: disk_hint,
        },
    ];

    SystemInfo {
        metrics,
        verdict,
        verdict_label: fmt_verdict_label(verdict).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_good_threshold() {
        // 8 * 1024^3 = exactly the Good boundary
        assert_eq!(rate_ram(Some(8 * 1024 * 1024 * 1024)).0, Status::Good);
        assert_eq!(rate_ram(Some(16 * 1024 * 1024 * 1024)).0, Status::Good);
    }

    #[test]
    fn ram_marginal_threshold() {
        assert_eq!(rate_ram(Some(4 * 1024 * 1024 * 1024)).0, Status::Marginal);
        assert_eq!(rate_ram(Some(7 * 1024 * 1024 * 1024)).0, Status::Marginal);
    }

    #[test]
    fn ram_insufficient_threshold() {
        assert_eq!(rate_ram(Some(1_000_000_000)).0, Status::Insufficient);
        assert_eq!(
            rate_ram(Some(4 * 1024 * 1024 * 1024 - 1)).0,
            Status::Insufficient
        );
    }

    #[test]
    fn ram_unknown() {
        assert_eq!(rate_ram(None).0, Status::Unknown);
    }

    #[test]
    fn cpu_good_threshold() {
        assert_eq!(rate_cpu(Some(4), Some(4)).0, Status::Good);
        assert_eq!(rate_cpu(Some(16), Some(8)).0, Status::Good);
    }

    #[test]
    fn cpu_marginal_threshold() {
        assert_eq!(rate_cpu(Some(2), Some(2)).0, Status::Marginal);
        assert_eq!(rate_cpu(Some(3), None).0, Status::Marginal);
    }

    #[test]
    fn cpu_insufficient_threshold() {
        assert_eq!(rate_cpu(Some(1), Some(1)).0, Status::Insufficient);
    }

    #[test]
    fn cpu_unknown() {
        assert_eq!(rate_cpu(None, Some(8)).0, Status::Unknown);
        assert_eq!(rate_cpu(None, None).0, Status::Unknown);
    }

    #[test]
    fn disk_good() {
        assert_eq!(rate_disk(Some(5 * 1024 * 1024 * 1024)).0, Status::Good);
        assert_eq!(rate_disk(Some(50_000_000_000)).0, Status::Good);
    }

    #[test]
    fn disk_marginal() {
        assert_eq!(rate_disk(Some(1_000_000)).0, Status::Marginal);
        assert_eq!(
            rate_disk(Some(5 * 1024 * 1024 * 1024 - 1)).0,
            Status::Marginal
        );
    }

    #[test]
    fn disk_unknown() {
        assert_eq!(rate_disk(None).0, Status::Unknown);
    }

    #[test]
    fn worst_status_ordering() {
        // Good is the floor only when at least one metric was actually rated.
        assert_eq!(worst_status(&[Status::Good]), Status::Good);
        assert_eq!(
            worst_status(&[Status::Marginal, Status::Good]),
            Status::Marginal
        );
        assert_eq!(
            worst_status(&[Status::Insufficient, Status::Good]),
            Status::Insufficient
        );
        assert_eq!(
            worst_status(&[Status::Good, Status::Unknown, Status::Marginal]),
            Status::Marginal
        );
        // Insufficient beats everything.
        assert_eq!(
            worst_status(&[Status::Unknown, Status::Marginal, Status::Insufficient]),
            Status::Insufficient
        );
    }

    #[test]
    fn worst_status_all_unknown_is_unknown() {
        // Nothing measured must NOT read as Good ("System ready").
        assert_eq!(worst_status(&[]), Status::Unknown);
        assert_eq!(worst_status(&[Status::Unknown]), Status::Unknown);
        assert_eq!(
            worst_status(&[Status::Unknown, Status::Unknown, Status::Unknown]),
            Status::Unknown
        );
    }

    #[test]
    fn fmt_human_readable_bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(500), "500 B");
        assert_eq!(fmt_bytes(1_048_576), "1.0 MiB");
        assert_eq!(fmt_bytes(8_589_934_592), "8.0 GiB");
    }
}
