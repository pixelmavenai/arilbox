//! Types shared by all container backends.

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

use arilbox_core::config::TemplateRef;
use arilbox_core::{BridgeName, ContainerName, SnapshotName, StorageName};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Lifecycle state of a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    /// Not running.
    Stopped,
    /// Booting.
    Starting,
    /// Running.
    Running,
    /// Shutting down.
    Stopping,
    /// Frozen (cgroup freezer).
    Frozen,
    /// State could not be determined.
    Error,
}

impl fmt::Display for ContainerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ContainerState::Stopped => "stopped",
            ContainerState::Starting => "starting",
            ContainerState::Running => "running",
            ContainerState::Stopping => "stopping",
            ContainerState::Frozen => "frozen",
            ContainerState::Error => "error",
        })
    }
}

/// Runtime and configuration summary of a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerInfo {
    /// Name.
    pub name: ContainerName,
    /// Current state.
    pub state: ContainerState,
    /// IPv4 addresses (running containers only).
    pub ipv4: Vec<Ipv4Addr>,
    /// IPv6 addresses (running containers only).
    pub ipv6: Vec<Ipv6Addr>,
    /// Init PID on the host (running containers only).
    pub pid: Option<u32>,
    /// Total CPU time used.
    pub cpu_usage_ns: u64,
    /// Current memory use.
    pub memory_bytes: u64,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Template it was created from.
    pub template: TemplateRef,
    /// Whether the NVIDIA GPU is shared with it.
    pub gpu: bool,
    /// Catalog app installed in it, if any.
    pub app_id: Option<String>,
}

/// Resource limits (`SPEC.md` §5.4). `None` means unlimited / backend default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// CPU cores (`cpu.max` quota).
    pub cores: Option<u32>,
    /// Memory in MiB (`memory.max`).
    pub memory_mb: Option<u64>,
    /// Swap in MiB (`memory.swap.max`).
    pub swap_mb: Option<u64>,
    /// Disk in GiB (ZFS `refquota`; ignored on `dir` pools).
    pub disk_gb: Option<u64>,
    /// Process limit (`pids.max`), default [`Limits::DEFAULT_PIDS_MAX`].
    pub pids_max: Option<u32>,
}

impl Limits {
    /// `pids.max` when none is given.
    pub const DEFAULT_PIDS_MAX: u32 = 4096;
}

/// How a container gets its address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// DHCP from the managed bridge's dnsmasq (or the LAN for a host bridge).
    #[default]
    Dhcp,
}

/// Network attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// Bridge to attach the veth to.
    pub bridge: BridgeName,
    /// Addressing mode.
    #[serde(default)]
    pub mode: NetworkMode,
}

/// Everything needed to create a container. Volumes are added in M4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSpec {
    /// Name.
    pub name: ContainerName,
    /// Base template.
    pub template: TemplateRef,
    /// Storage pool for the rootfs.
    pub storage: StorageName,
    /// Initial limits.
    pub limits: Limits,
    /// Network attachment.
    pub network: NetworkSpec,
    /// Share the NVIDIA GPU.
    pub gpu: bool,
    /// Allow nested containers (Podman inside).
    pub nesting: bool,
    /// Start on host boot.
    pub autostart: bool,
}

/// A snapshot of a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// Snapshot name.
    pub name: SnapshotName,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Result of running a command in a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Exit code.
    pub exit_code: i32,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Captured stderr.
    pub stderr: Vec<u8>,
}
