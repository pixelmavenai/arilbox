//! Host information and capability detection (`SPEC.md` §7.2, §10, §13).
//!
//! Everything is read from procfs, sysfs and device nodes relative to a configurable root, so
//! tests can point the probe at a fake tree.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;

use crate::error::{Error, Result};

/// Static facts about the host (`GET /api/v1/host`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostInfo {
    /// Kernel hostname.
    pub hostname: String,
    /// OS name from `/etc/os-release` (`PRETTY_NAME`).
    pub os: String,
    /// Kernel release, e.g. `6.12.48+deb13-amd64`.
    pub kernel: String,
    /// Seconds since boot.
    pub uptime_s: u64,
    /// Number of logical CPUs.
    pub cpu_count: u32,
    /// Total RAM in bytes.
    pub memory_total_bytes: u64,
    /// Arilbox version.
    pub platform_version: String,
}

/// Current utilisation (`GET /api/v1/host/stats`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostStats {
    /// CPU busy time in percent of all CPUs since the previous call (since boot on the first).
    pub cpu_percent: f64,
    /// Total RAM in bytes.
    pub memory_total_bytes: u64,
    /// RAM in use: total minus `MemAvailable`.
    pub memory_used_bytes: u64,
    /// Total swap in bytes.
    pub swap_total_bytes: u64,
    /// Swap in use.
    pub swap_used_bytes: u64,
    /// Load averages over 1, 5 and 15 minutes.
    pub load: [f64; 3],
    /// Seconds since boot.
    pub uptime_s: u64,
}

/// Optional host features (`GET /api/v1/host/capabilities`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct Capabilities {
    /// ZFS kernel module loaded (`/dev/zfs`).
    pub zfs: bool,
    /// NVIDIA driver usable: `nvidia-smi` installed and `/dev/nvidia0` present.
    pub nvidia: bool,
    /// `/dev/kvm` present (only needed for future VMs).
    pub kvm: bool,
    /// Unified cgroup v2 hierarchy mounted at `/sys/fs/cgroup`.
    pub cgroup_v2: bool,
}

/// Source of host information. Abstracted so API tests don't depend on the machine they run on.
pub trait HostProbe: Send + Sync {
    /// Reads host information.
    fn host_info(&self) -> Result<HostInfo>;
    /// Detects capabilities. Never fails: anything unreadable counts as absent.
    fn capabilities(&self) -> Capabilities;
    /// Reads current utilisation.
    fn stats(&self) -> Result<HostStats>;
}

/// Reads the real host (root `/`) or a fake tree.
#[derive(Debug, Clone)]
pub struct SystemProbe {
    root: PathBuf,
    /// CPU counters from the previous [`HostProbe::stats`] call.
    last_cpu: Arc<Mutex<Option<CpuTimes>>>,
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new("/")
    }
}

impl SystemProbe {
    /// A probe that reads files under `root` instead of `/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            last_cpu: Arc::default(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn read(&self, rel: &str) -> Result<String> {
        let path = self.path(rel);
        fs::read_to_string(&path).map_err(|source| Error::Io {
            context: format!("reading {}", path.display()),
            source,
        })
    }

    fn exists(&self, rel: &str) -> bool {
        self.path(rel).exists()
    }
}

impl HostProbe for SystemProbe {
    fn host_info(&self) -> Result<HostInfo> {
        Ok(HostInfo {
            hostname: self.read("proc/sys/kernel/hostname")?.trim().to_owned(),
            // os-release may be absent in minimal environments; that is not fatal.
            os: self
                .read("etc/os-release")
                .ok()
                .and_then(|s| parse_os_release(&s))
                .unwrap_or_else(|| "unknown".into()),
            kernel: self.read("proc/sys/kernel/osrelease")?.trim().to_owned(),
            uptime_s: parse_uptime(&self.read("proc/uptime")?)?,
            cpu_count: parse_cpu_count(&self.read("proc/cpuinfo")?),
            memory_total_bytes: parse_mem_total(&self.read("proc/meminfo")?)?,
            platform_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        let nvidia_smi = ["usr/bin/nvidia-smi", "usr/sbin/nvidia-smi"]
            .iter()
            .any(|p| self.exists(p));
        Capabilities {
            zfs: self.exists("dev/zfs"),
            nvidia: nvidia_smi && self.exists("dev/nvidia0"),
            kvm: self.exists("dev/kvm"),
            cgroup_v2: self.exists("sys/fs/cgroup/cgroup.controllers"),
        }
    }

    fn stats(&self) -> Result<HostStats> {
        let now = parse_cpu_times(&self.read("proc/stat")?)?;
        let previous = self
            .last_cpu
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(now);
        let mem = parse_meminfo(&self.read("proc/meminfo")?)?;
        Ok(HostStats {
            cpu_percent: now.busy_percent_since(previous.unwrap_or_default()),
            memory_total_bytes: mem.total,
            memory_used_bytes: mem.total.saturating_sub(mem.available),
            swap_total_bytes: mem.swap_total,
            swap_used_bytes: mem.swap_total.saturating_sub(mem.swap_free),
            load: parse_loadavg(&self.read("proc/loadavg")?)?,
            uptime_s: parse_uptime(&self.read("proc/uptime")?)?,
        })
    }
}

/// Aggregate CPU counters from the `cpu` line of `/proc/stat`, in clock ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CpuTimes {
    total: u64,
    idle: u64,
}

impl CpuTimes {
    /// Busy share of the time between `earlier` and `self`, 0–100 with one decimal.
    fn busy_percent_since(self, earlier: CpuTimes) -> f64 {
        let total = self.total.saturating_sub(earlier.total);
        let idle = self.idle.saturating_sub(earlier.idle);
        if total == 0 {
            return 0.0;
        }
        // Tick counts fit f64 exactly for centuries of uptime.
        #[allow(clippy::cast_precision_loss)]
        let percent = (total.saturating_sub(idle)) as f64 * 100.0 / total as f64;
        (percent * 10.0).round() / 10.0
    }
}

/// `user nice system idle iowait irq softirq steal` from the `cpu` line (guest time is already
/// counted in user).
fn parse_cpu_times(text: &str) -> Result<CpuTimes> {
    let fields: Vec<u64> = text
        .lines()
        .find_map(|l| l.strip_prefix("cpu "))
        .ok_or_else(|| parse_error("stat"))?
        .split_whitespace()
        .take(8)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| parse_error("stat"))?;
    if fields.len() < 5 {
        return Err(parse_error("stat"));
    }
    Ok(CpuTimes {
        total: fields.iter().sum(),
        idle: fields[3] + fields[4],
    })
}

/// The `/proc/meminfo` fields [`HostStats`] needs, in bytes.
#[derive(Debug, PartialEq, Eq)]
struct MemInfo {
    total: u64,
    available: u64,
    swap_total: u64,
    swap_free: u64,
}

fn parse_meminfo(text: &str) -> Result<MemInfo> {
    let field = |key: &str| -> Result<u64> {
        let value = text
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
            .ok_or_else(|| parse_error("meminfo"))?;
        let kib: u64 = value
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .map_err(|_| parse_error("meminfo"))?;
        kib.checked_mul(1024).ok_or_else(|| parse_error("meminfo"))
    };
    Ok(MemInfo {
        total: field("MemTotal")?,
        available: field("MemAvailable")?,
        swap_total: field("SwapTotal")?,
        swap_free: field("SwapFree")?,
    })
}

/// First three fields of `/proc/loadavg`.
fn parse_loadavg(text: &str) -> Result<[f64; 3]> {
    let mut it = text.split_whitespace().map(str::parse::<f64>);
    let mut next = || {
        it.next()
            .and_then(std::result::Result::ok)
            .ok_or_else(|| parse_error("loadavg"))
    };
    Ok([next()?, next()?, next()?])
}

fn parse_error(what: &str) -> Error {
    Error::Io {
        context: format!("parsing {what}"),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, "unexpected format"),
    }
}

/// `PRETTY_NAME` from os-release, with optional quotes removed.
fn parse_os_release(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_owned())
        .filter(|v| !v.is_empty())
}

/// First field of `/proc/uptime`, truncated to whole seconds.
fn parse_uptime(text: &str) -> Result<u64> {
    let first = text
        .split_whitespace()
        .next()
        .ok_or_else(|| parse_error("uptime"))?;
    let secs = first.split('.').next().unwrap_or(first);
    secs.parse().map_err(|_| parse_error("uptime"))
}

/// Count of `processor` entries in `/proc/cpuinfo`.
fn parse_cpu_count(text: &str) -> u32 {
    let count = text
        .lines()
        .filter(|l| l.split(':').next().is_some_and(|k| k.trim() == "processor"))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `MemTotal` from `/proc/meminfo`, in bytes.
fn parse_mem_total(text: &str) -> Result<u64> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .ok_or_else(|| parse_error("meminfo"))?;
    let kib: u64 = line
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .map_err(|_| parse_error("meminfo"))?;
    kib.checked_mul(1024).ok_or_else(|| parse_error("meminfo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temp directory removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("arilbox-test-host-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, rel: &str, contents: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn populated(tag: &str) -> TempRoot {
        let root = TempRoot::new(tag);
        root.write("proc/sys/kernel/hostname", "devbox\n");
        root.write("proc/sys/kernel/osrelease", "6.12.48+deb13-amd64\n");
        root.write("proc/uptime", "12345.67 98765.43\n");
        root.write(
            "proc/cpuinfo",
            "processor\t: 0\nmodel name\t: X\n\nprocessor\t: 1\nmodel name\t: X\n",
        );
        root.write(
            "proc/meminfo",
            "MemTotal:       16384000 kB\nMemFree: 1 kB\n",
        );
        root.write(
            "etc/os-release",
            "NAME=\"Debian GNU/Linux\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n",
        );
        root
    }

    #[test]
    fn reads_host_info_from_tree() {
        let root = populated("info");
        let info = SystemProbe::new(&root.0).host_info().unwrap();
        assert_eq!(
            info,
            HostInfo {
                hostname: "devbox".into(),
                os: "Debian GNU/Linux 13 (trixie)".into(),
                kernel: "6.12.48+deb13-amd64".into(),
                uptime_s: 12345,
                cpu_count: 2,
                memory_total_bytes: 16_384_000 * 1024,
                platform_version: env!("CARGO_PKG_VERSION").into(),
            }
        );
    }

    #[test]
    fn missing_os_release_is_not_fatal() {
        let root = populated("noos");
        fs::remove_file(root.0.join("etc/os-release")).unwrap();
        assert_eq!(SystemProbe::new(&root.0).host_info().unwrap().os, "unknown");
    }

    #[test]
    fn missing_proc_file_is_an_error() {
        let root = populated("noproc");
        fs::remove_file(root.0.join("proc/meminfo")).unwrap();
        assert!(SystemProbe::new(&root.0).host_info().is_err());
    }

    #[test]
    fn capabilities_detection() {
        let root = TempRoot::new("caps");
        let probe = SystemProbe::new(&root.0);
        assert_eq!(probe.capabilities(), Capabilities::default());

        root.write("dev/zfs", "");
        root.write("dev/kvm", "");
        root.write("sys/fs/cgroup/cgroup.controllers", "cpu memory pids\n");
        root.write("dev/nvidia0", "");
        let caps = probe.capabilities();
        assert!(caps.zfs && caps.kvm && caps.cgroup_v2);
        assert!(!caps.nvidia, "device node alone is not enough");

        root.write("usr/bin/nvidia-smi", "");
        assert!(probe.capabilities().nvidia);
    }

    #[test]
    fn parsers_reject_garbage() {
        assert!(parse_uptime("").is_err());
        assert!(parse_uptime("abc").is_err());
        assert!(parse_mem_total("MemFree: 1 kB").is_err());
        assert!(parse_mem_total("MemTotal: lots kB").is_err());
        assert_eq!(
            parse_os_release("PRETTY_NAME=Alpine"),
            Some("Alpine".into())
        );
        assert_eq!(parse_os_release("PRETTY_NAME=\"\""), None);
    }

    #[test]
    fn stats_from_tree() {
        let root = populated("stats");
        root.write(
            "proc/meminfo",
            "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 400 kB\n\
             SwapTotal: 200 kB\nSwapFree: 150 kB\n",
        );
        root.write("proc/loadavg", "0.50 1.25 2.00 1/123 4567\n");
        root.write(
            "proc/stat",
            "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4 5\n",
        );
        let probe = SystemProbe::new(&root.0);
        let s = probe.stats().unwrap();
        assert_eq!(s.cpu_percent, 20.0, "since boot: 200 busy of 1000");
        assert_eq!(s.memory_total_bytes, 1000 * 1024);
        assert_eq!(s.memory_used_bytes, 600 * 1024);
        assert_eq!(
            (s.swap_total_bytes, s.swap_used_bytes),
            (200 * 1024, 50 * 1024)
        );
        assert_eq!(s.load, [0.5, 1.25, 2.0]);
        assert_eq!(s.uptime_s, 12345);

        // Next call measures only the delta: +300 busy, +100 idle.
        root.write("proc/stat", "cpu  250 0 250 800 100 0 0 0 0 0\n");
        assert_eq!(probe.stats().unwrap().cpu_percent, 75.0);
    }

    #[test]
    fn stats_parsers_reject_garbage() {
        assert!(parse_cpu_times("cpu0 1 2 3 4 5").is_err());
        assert!(parse_cpu_times("cpu  1 2").is_err());
        assert!(parse_cpu_times("cpu  1 x 3 4 5").is_err());
        assert!(parse_loadavg("1.0 2.0").is_err());
        assert!(parse_meminfo("MemTotal: 1 kB").is_err());
        assert_eq!(
            CpuTimes { total: 5, idle: 5 }.busy_percent_since(CpuTimes { total: 5, idle: 5 }),
            0.0
        );
    }

    #[test]
    fn real_host_smoke() {
        // Linux CI and dev hosts always have these procfs files.
        let info = SystemProbe::default().host_info().unwrap();
        assert!(info.cpu_count >= 1);
        assert!(info.memory_total_bytes > 0);
        let stats = SystemProbe::default().stats().unwrap();
        assert!((0.0..=100.0).contains(&stats.cpu_percent));
        assert!(stats.memory_used_bytes <= stats.memory_total_bytes);
    }
}
