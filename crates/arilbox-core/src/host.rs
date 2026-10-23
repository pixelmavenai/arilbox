//! Host information and capability detection (`SPEC.md` §7.2, §10, §13).
//!
//! Everything is read from procfs, sysfs and device nodes relative to a configurable root, so
//! tests can point the probe at a fake tree.

use std::fs;
use std::path::PathBuf;

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
}

/// Reads the real host (root `/`) or a fake tree.
#[derive(Debug, Clone)]
pub struct SystemProbe {
    root: PathBuf,
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new("/")
    }
}

impl SystemProbe {
    /// A probe that reads files under `root` instead of `/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
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
    fn real_host_smoke() {
        // Linux CI and dev hosts always have these procfs files.
        let info = SystemProbe::default().host_info().unwrap();
        assert!(info.cpu_count >= 1);
        assert!(info.memory_total_bytes > 0);
    }
}
