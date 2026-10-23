//! LXC config generation (`SPEC.md` §5.3, §5.4, §9; `.claude/skills/lxc-config`).
//!
//! The config is rendered from typed data by the pure function [`render_config`], which is
//! deterministic and covered by golden tests in `tests/fixtures/`. Nothing a user types is
//! copied into the file except validated identifiers. No `lxc.include`: the security baseline
//! that LXC's distro includes would provide is written out as constants ([`BASELINE`]).

use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::str::FromStr;

use arilbox_core::{BridgeName, ContainerName, DatasetPath};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::Limits;

/// First line of every generated config.
pub const HEADER: &str = "# Managed by arilboxd — manual edits will be overwritten";

/// Subordinate ID range for every container (`SPEC.md` §5.3). Privileged containers are not
/// supported, so this never varies.
const IDMAP: [&str; 2] = ["u 0 100000 65536", "g 0 100000 65536"];

/// What Debian's `common.conf` + `userns.conf` give an unprivileged container, inlined.
/// Device and capability rules are deliberately absent, as in upstream `userns.conf`: the user
/// namespace already denies them.
const BASELINE: &[(&str, &str)] = &[
    (
        "lxc.seccomp.profile",
        "/usr/share/lxc/config/common.seccomp",
    ),
    ("lxc.mount.auto", "cgroup:mixed proc:mixed sys:mixed"),
    (
        "lxc.mount.entry",
        "/sys/fs/fuse/connections sys/fs/fuse/connections none bind,optional 0 0",
    ),
    ("lxc.pty.max", "1024"),
    ("lxc.tty.max", "4"),
];

/// Extra mounts for `nesting = true` (from upstream `nesting.conf`, without its AppArmor line:
/// the generated profile with `allow_nesting = 1` replaces it).
const NESTING_MOUNTS: &[&str] = &[
    "proc dev/.lxc/proc proc create=dir,optional 0 0",
    "sys dev/.lxc/sys sysfs create=dir,optional 0 0",
];

/// CPU period for `cpu.max`, in microseconds.
const CPU_PERIOD: u64 = 100_000;

/// A MAC address. Containers get a fixed random one at creation so DHCP leases survive
/// restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// A random address with LXC's `00:16:3e` prefix, read from `/dev/urandom`.
    pub fn random() -> std::io::Result<Self> {
        use std::io::Read;
        let mut tail = [0u8; 3];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut tail)?;
        Ok(Self([0x00, 0x16, 0x3e, tail[0], tail[1], tail[2]]))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl FromStr for MacAddr {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let invalid = || Error::Backend("invalid MAC address".into());
        let mut out = [0u8; 6];
        let mut parts = s.split(':');
        for byte in &mut out {
            let part = parts.next().ok_or_else(invalid)?;
            if part.len() != 2 {
                return Err(invalid());
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| invalid())?;
        }
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(Self(out))
    }
}

impl TryFrom<String> for MacAddr {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}

impl From<MacAddr> for String {
    fn from(m: MacAddr) -> String {
        m.to_string()
    }
}

/// Where a container's root filesystem lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rootfs {
    /// A directory (absolute path from a configured `dir` pool).
    Dir {
        /// Rootfs directory.
        path: PathBuf,
    },
    /// A ZFS dataset.
    Zfs {
        /// Rootfs dataset.
        dataset: DatasetPath,
    },
}

impl Rootfs {
    fn lxc_value(&self) -> String {
        match self {
            Rootfs::Dir { path } => format!("dir:{}", path.display()),
            Rootfs::Zfs { dataset } => format!("zfs:{dataset}"),
        }
    }
}

/// Everything that goes into a container's config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerConfig {
    /// Container name (also the hostname).
    pub name: ContainerName,
    /// LXC architecture, e.g. `amd64`.
    pub arch: &'static str,
    /// Root filesystem.
    pub rootfs: Rootfs,
    /// Resource limits.
    pub limits: Limits,
    /// `lxc.start.auto`.
    pub autostart: bool,
    /// Allow nested containers.
    pub nesting: bool,
    /// Bridge for `eth0`.
    pub bridge: BridgeName,
    /// MAC of `eth0`.
    pub hwaddr: MacAddr,
}

/// Renders the config file. Pure and deterministic: same input, byte-identical output.
pub fn render_config(c: &ContainerConfig) -> Result<String> {
    let memory = match c.limits.memory_mb {
        Some(mb) => mib_to_bytes(mb)?.to_string(),
        None => "max".to_owned(),
    };
    let swap = match c.limits.swap_mb {
        Some(mb) => mib_to_bytes(mb)?.to_string(),
        None => "max".to_owned(),
    };
    let cpu = match c.limits.cores {
        Some(cores) => u64::from(cores)
            .checked_mul(CPU_PERIOD)
            .ok_or_else(|| Error::Backend("cpu quota overflow".into()))?
            .to_string(),
        None => "max".to_owned(),
    };
    let pids = c.limits.pids_max.unwrap_or(Limits::DEFAULT_PIDS_MAX);

    let mut lines: Vec<(&str, String)> = vec![
        ("lxc.uts.name", c.name.to_string()),
        ("lxc.arch", c.arch.to_owned()),
        ("lxc.rootfs.path", c.rootfs.lxc_value()),
    ];
    lines.extend(IDMAP.iter().map(|v| ("lxc.idmap", (*v).to_owned())));
    lines.push(("lxc.apparmor.profile", "generated".to_owned()));
    lines.push((
        "lxc.apparmor.allow_nesting",
        if c.nesting { "1" } else { "0" }.to_owned(),
    ));
    lines.extend(BASELINE.iter().map(|(k, v)| (*k, (*v).to_owned())));
    if c.nesting {
        lines.extend(
            NESTING_MOUNTS
                .iter()
                .map(|v| ("lxc.mount.entry", (*v).to_owned())),
        );
    }
    lines.extend([
        ("lxc.cgroup2.memory.max", memory),
        ("lxc.cgroup2.memory.swap.max", swap),
        ("lxc.cgroup2.cpu.max", format!("{cpu} {CPU_PERIOD}")),
        ("lxc.cgroup2.pids.max", pids.to_string()),
        (
            "lxc.start.auto",
            if c.autostart { "1" } else { "0" }.to_owned(),
        ),
        ("lxc.net.0.type", "veth".to_owned()),
        ("lxc.net.0.link", c.bridge.to_string()),
        ("lxc.net.0.flags", "up".to_owned()),
        ("lxc.net.0.name", "eth0".to_owned()),
        ("lxc.net.0.hwaddr", c.hwaddr.to_string()),
    ]);

    let mut out = String::new();
    out.push_str(HEADER);
    out.push('\n');
    for (key, value) in lines {
        check_value(&value)?;
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{key} = {value}");
    }
    Ok(out)
}

/// The config passed to `lxc-create -f`: the full config without `lxc.rootfs.path`. If the
/// seed named a rootfs, LXC would expect it to exist and skip creating storage; without it,
/// LXC creates the rootfs from `-B`/`--dir` and unpacks the image with the idmap.
pub fn seed_config(rendered: &str) -> String {
    rendered
        .lines()
        .filter(|line| !line.starts_with("lxc.rootfs.path "))
        .flat_map(|line| [line, "\n"])
        .collect()
}

fn mib_to_bytes(mb: u64) -> Result<u64> {
    mb.checked_mul(1024 * 1024)
        .ok_or_else(|| Error::Backend("memory size overflow".into()))
}

/// Last line of defence: no value may break out of its line or start a comment.
fn check_value(value: &str) -> Result<()> {
    if value.contains(['\n', '\r', '\0', '#']) {
        return Err(Error::Backend(
            "refusing to render unsafe config value".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ContainerConfig {
        ContainerConfig {
            name: ContainerName::new("web").unwrap(),
            arch: "amd64",
            rootfs: Rootfs::Dir {
                path: "/var/lib/arilbox/storage/containers/web/rootfs".into(),
            },
            limits: Limits::default(),
            autostart: false,
            nesting: false,
            bridge: BridgeName::new("arilbox0").unwrap(),
            hwaddr: MacAddr([0x00, 0x16, 0x3e, 0x4a, 0x91, 0x0c]),
        }
    }

    fn golden(name: &str, config: &ContainerConfig) {
        let path = format!("{}/tests/fixtures/{name}.conf", env!("CARGO_MANIFEST_DIR"));
        let expected = std::fs::read_to_string(&path).unwrap();
        assert_eq!(render_config(config).unwrap(), expected, "fixture {path}");
    }

    #[test]
    fn golden_default() {
        golden("default", &base());
    }

    #[test]
    fn golden_limits() {
        let mut c = base();
        c.limits = Limits {
            cores: Some(2),
            memory_mb: Some(1024),
            swap_mb: Some(0),
            disk_gb: Some(20),
            pids_max: Some(2048),
        };
        c.autostart = true;
        golden("limits", &c);
    }

    #[test]
    fn golden_nesting_zfs() {
        let mut c = base();
        c.nesting = true;
        c.rootfs = Rootfs::Zfs {
            dataset: DatasetPath::new("tank/arilbox/containers/web").unwrap(),
        };
        golden("nesting-zfs", &c);
    }

    #[test]
    fn seed_has_everything_but_the_rootfs() {
        let full = render_config(&base()).unwrap();
        let seed = seed_config(&full);
        assert!(!seed.contains("lxc.rootfs"), "{seed}");
        assert_eq!(seed.lines().count(), full.lines().count() - 1);
        assert!(seed.contains("lxc.idmap = u 0 100000 65536\n"));
        assert!(seed.ends_with("lxc.net.0.hwaddr = 00:16:3e:4a:91:0c\n"));
    }

    #[test]
    fn deterministic() {
        assert_eq!(
            render_config(&base()).unwrap(),
            render_config(&base()).unwrap()
        );
    }

    #[test]
    fn never_renders_forbidden_lines() {
        for nesting in [false, true] {
            let mut c = base();
            c.nesting = nesting;
            let text = render_config(&c).unwrap();
            for forbidden in [
                "lxc.include",
                "unconfined",
                "lxc.cap.",
                "lxc.cgroup2.devices",
                "lxc.hook",
                "lxc.init.cmd",
                "lxc.execute.cmd",
                "lxc.environment",
            ] {
                assert!(!text.contains(forbidden), "{forbidden} in:\n{text}");
            }
            assert_eq!(text.matches("lxc.idmap = ").count(), 2);
            assert!(text.contains("lxc.apparmor.profile = generated\n"));
        }
    }

    #[test]
    fn unsafe_rootfs_path_is_refused() {
        let mut c = base();
        c.rootfs = Rootfs::Dir {
            path: "/x\nlxc.include = /etc/evil".into(),
        };
        assert!(render_config(&c).is_err());
        c.rootfs = Rootfs::Dir {
            path: "/x#y".into(),
        };
        assert!(render_config(&c).is_err());
    }

    #[test]
    fn hostile_names_never_reach_the_renderer() {
        for bad in ["../etc", "/etc/shadow", "a\nlxc.include=/x", "a b"] {
            assert!(ContainerName::new(bad).is_err());
            assert!(BridgeName::new(bad).is_err());
        }
    }

    #[test]
    fn overflow_is_an_error() {
        let mut c = base();
        c.limits.memory_mb = Some(u64::MAX);
        assert!(render_config(&c).is_err());
    }

    #[test]
    fn mac_roundtrip() {
        let mac: MacAddr = "00:16:3e:4a:91:0c".parse().unwrap();
        assert_eq!(mac.to_string(), "00:16:3e:4a:91:0c");
        for bad in [
            "",
            "00:16:3e:4a:91",
            "00:16:3e:4a:91:0c:00",
            "0:16:3e:4a:91:0c",
            "zz:16:3e:4a:91:0c",
        ] {
            assert!(bad.parse::<MacAddr>().is_err(), "{bad}");
        }
        let random = MacAddr::random().unwrap();
        assert_eq!(&random.0[..3], &[0x00, 0x16, 0x3e]);
    }
}
