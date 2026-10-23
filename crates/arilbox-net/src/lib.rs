//! Managed bridge networking (`SPEC.md` §9).
//!
//! - A Linux bridge (default `arilbox0`) with the configured address, created with `ip`.
//! - Outbound NAT via an nftables `masquerade` rule in a dedicated table (default `arilbox`),
//!   loaded atomically with `nft -f -`.
//! - DHCP and DNS for containers from a `dnsmasq` bound only to the bridge, supervised by the
//!   daemon and restarted if it exits.
//!
//! All commands go through `arilbox_core::exec`; argument vectors and the ruleset are built by
//! pure functions and unit-tested. Nothing here forwards inbound ports.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arilbox_core::BridgeName;
use arilbox_core::config::Ipv4Cidr;
use arilbox_core::exec::{CommandSpec, Exec, ExecError, Program, RealExec};

/// Errors from network setup.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A command failed.
    #[error(transparent)]
    Exec(#[from] ExecError),
    /// An interface with the bridge's name exists but is not a bridge.
    #[error("interface '{0}' exists but is not a bridge")]
    NotABridge(BridgeName),
    /// Reading or writing a kernel setting or file failed.
    #[error("{0}")]
    Io(String),
}

/// Result alias for `arilbox-net`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything needed to set up the managed network.
#[derive(Debug, Clone)]
pub struct ManagedNetwork {
    /// Bridge name.
    pub bridge: BridgeName,
    /// Bridge address and prefix.
    pub subnet: Ipv4Cidr,
    /// nftables table name. A constant: `arilbox` in production, `arilbox_test` in tests.
    pub nft_table: &'static str,
    /// Directory for the dnsmasq lease and pid files.
    pub state_dir: PathBuf,
}

/// Root of sysfs/procfs lookups; `/` except in tests.
#[derive(Debug, Clone)]
pub struct Sys {
    root: PathBuf,
}

impl Default for Sys {
    fn default() -> Self {
        Self { root: "/".into() }
    }
}

impl Sys {
    /// Uses `root` instead of `/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn iface_dir(&self, name: &BridgeName) -> PathBuf {
        self.root.join("sys/class/net").join(name.as_str())
    }

    /// Whether an interface with this name exists.
    pub fn interface_exists(&self, name: &BridgeName) -> bool {
        self.iface_dir(name).exists()
    }

    /// Whether `name` exists and is a bridge (used to validate user-chosen host bridges).
    pub fn is_bridge(&self, name: &BridgeName) -> bool {
        self.iface_dir(name).join("bridge").is_dir()
    }

    fn ip_forward_path(&self) -> PathBuf {
        self.root.join("proc/sys/net/ipv4/ip_forward")
    }
}

// ---- pure builders --------------------------------------------------------------------------

fn netmask(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0))
}

/// Network address of `cidr`, e.g. `10.88.0.0` for `10.88.0.1/24`.
pub fn network_addr(cidr: &Ipv4Cidr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(cidr.addr) & u32::from(netmask(cidr.prefix)))
}

/// DHCP pool: every host address in the subnet above the bridge address.
/// Returns `(first, last, netmask)`, or `None` if no addresses are left.
pub fn dhcp_range(cidr: &Ipv4Cidr) -> Option<(Ipv4Addr, Ipv4Addr, Ipv4Addr)> {
    let mask = u32::from(netmask(cidr.prefix));
    let network = u32::from(cidr.addr) & mask;
    let broadcast = network | !mask;
    let first = u32::from(cidr.addr).checked_add(1)?;
    let last = broadcast.checked_sub(1)?;
    (first <= last).then(|| (first.into(), last.into(), mask.into()))
}

/// `ip link add name <bridge> type bridge`
pub fn bridge_add_cmd(bridge: &BridgeName) -> CommandSpec {
    CommandSpec::new(
        Program::Ip,
        ["link", "add", "name", bridge.as_str(), "type", "bridge"],
    )
}

/// `ip addr replace <cidr> dev <bridge>` (idempotent)
pub fn bridge_addr_cmd(bridge: &BridgeName, cidr: &Ipv4Cidr) -> CommandSpec {
    CommandSpec::new(
        Program::Ip,
        [
            "addr".to_owned(),
            "replace".to_owned(),
            cidr.to_string(),
            "dev".to_owned(),
            bridge.to_string(),
        ],
    )
}

/// `ip link set dev <bridge> up`
pub fn bridge_up_cmd(bridge: &BridgeName) -> CommandSpec {
    CommandSpec::new(Program::Ip, ["link", "set", "dev", bridge.as_str(), "up"])
}

/// `ip link del dev <bridge>` (tests only tear down their own bridge)
pub fn bridge_del_cmd(bridge: &BridgeName) -> CommandSpec {
    CommandSpec::new(Program::Ip, ["link", "del", "dev", bridge.as_str()])
}

/// The nftables ruleset. Declaring then deleting the table first makes loading idempotent and
/// atomic: the whole file is one transaction.
pub fn nft_ruleset(table: &str, cidr: &Ipv4Cidr) -> String {
    let net = format!("{}/{}", network_addr(cidr), cidr.prefix);
    format!(
        "table ip {table}\n\
         delete table ip {table}\n\
         table ip {table} {{\n\
         \tchain postrouting {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         \t\tip saddr {net} ip daddr != {net} masquerade\n\
         \t}}\n\
         }}\n"
    )
}

/// `nft -f -` with the ruleset on stdin.
pub fn nft_load_cmd(table: &str, cidr: &Ipv4Cidr) -> CommandSpec {
    CommandSpec::new(Program::Nft, ["-f", "-"]).stdin(nft_ruleset(table, cidr))
}

/// `nft delete table ip <table>` (tests only)
pub fn nft_delete_cmd(table: &str) -> CommandSpec {
    CommandSpec::new(Program::Nft, ["delete", "table", "ip", table])
}

/// dnsmasq in the foreground, bound only to the bridge, ignoring `/etc/dnsmasq.conf`.
pub fn dnsmasq_cmd(net: &ManagedNetwork) -> Option<CommandSpec> {
    let (first, last, mask) = dhcp_range(&net.subnet)?;
    let bridge = net.bridge.as_str();
    let state = |file: String| net.state_dir.join(file).display().to_string();
    let args = vec![
        "--keep-in-foreground".to_owned(),
        "--conf-file=".to_owned(),
        "--log-facility=-".to_owned(),
        "--strict-order".to_owned(),
        "--bind-interfaces".to_owned(),
        "--except-interface=lo".to_owned(),
        format!("--interface={bridge}"),
        format!("--listen-address={}", net.subnet.addr),
        format!("--dhcp-range={first},{last},{mask},1h"),
        "--dhcp-no-override".to_owned(),
        "--dhcp-authoritative".to_owned(),
        format!("--dhcp-leasefile={}", state(format!("{bridge}.leases"))),
        format!("--pid-file={}", state(format!("dnsmasq-{bridge}.pid"))),
    ];
    Some(CommandSpec::new(Program::Dnsmasq, args))
}

// ---- host changes ---------------------------------------------------------------------------

/// Creates and configures the bridge, enables IPv4 forwarding and loads the NAT rule.
/// Idempotent: safe to run on every daemon start.
pub async fn setup(exec: &dyn Exec, sys: &Sys, net: &ManagedNetwork) -> Result<()> {
    if sys.interface_exists(&net.bridge) {
        if !sys.is_bridge(&net.bridge) {
            return Err(Error::NotABridge(net.bridge.clone()));
        }
    } else {
        tracing::info!(bridge = %net.bridge, "creating bridge");
        exec.run(bridge_add_cmd(&net.bridge)).await?;
    }
    exec.run(bridge_addr_cmd(&net.bridge, &net.subnet)).await?;
    exec.run(bridge_up_cmd(&net.bridge)).await?;
    enable_ip_forward(sys)?;
    exec.run(nft_load_cmd(net.nft_table, &net.subnet)).await?;
    std::fs::create_dir_all(&net.state_dir)
        .map_err(|e| Error::Io(format!("creating {}: {e}", net.state_dir.display())))?;
    Ok(())
}

fn enable_ip_forward(sys: &Sys) -> Result<()> {
    let path = sys.ip_forward_path();
    let current = std::fs::read_to_string(&path)
        .map_err(|e| Error::Io(format!("reading {}: {e}", path.display())))?;
    if current.trim() != "1" {
        tracing::info!("enabling net.ipv4.ip_forward");
        std::fs::write(&path, "1")
            .map_err(|e| Error::Io(format!("writing {}: {e}", path.display())))?;
    }
    Ok(())
}

/// Runs dnsmasq for the bridge until the returned task is aborted or the runtime shuts down
/// (the child is killed when dropped). Restarts it with backoff if it exits.
pub fn supervise_dnsmasq(
    exec: Arc<RealExec>,
    net: &ManagedNetwork,
) -> Option<tokio::task::JoinHandle<()>> {
    let spec = dnsmasq_cmd(net)?;
    let bridge = net.bridge.clone();
    Some(tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = tokio::time::Instant::now();
            match exec.spawn_service(&spec) {
                Ok(mut child) => {
                    tracing::info!(%bridge, pid = ?child.id(), "dnsmasq started");
                    match child.wait().await {
                        Ok(status) => tracing::error!(%bridge, %status, "dnsmasq exited"),
                        Err(e) => tracing::error!(%bridge, error = %e, "waiting for dnsmasq"),
                    }
                }
                Err(e) => tracing::error!(%bridge, error = %e, "cannot start dnsmasq"),
            }
            // A run that lasted a while resets the backoff.
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(1);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use arilbox_core::exec::{ExecError, RecordingExec};

    use super::*;

    fn cidr(s: &str) -> Ipv4Cidr {
        Ipv4Cidr::try_from(s.to_owned()).unwrap()
    }

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn net(state: &Path) -> ManagedNetwork {
        ManagedNetwork {
            bridge: BridgeName::new("arilbox0").unwrap(),
            subnet: cidr("10.88.0.1/24"),
            nft_table: "arilbox",
            state_dir: state.to_owned(),
        }
    }

    #[test]
    fn address_math() {
        assert_eq!(
            network_addr(&cidr("10.88.0.1/24")),
            Ipv4Addr::new(10, 88, 0, 0)
        );
        assert_eq!(
            network_addr(&cidr("172.16.5.9/16")),
            Ipv4Addr::new(172, 16, 0, 0)
        );
        assert_eq!(
            dhcp_range(&cidr("10.88.0.1/24")),
            Some((
                Ipv4Addr::new(10, 88, 0, 2),
                Ipv4Addr::new(10, 88, 0, 254),
                Ipv4Addr::new(255, 255, 255, 0)
            ))
        );
        // Bridge at the top of the range leaves nothing above it.
        assert_eq!(dhcp_range(&cidr("10.88.0.254/24")), None);
        assert_eq!(
            dhcp_range(&cidr("10.0.0.1/30")),
            Some((
                Ipv4Addr::new(10, 0, 0, 2),
                Ipv4Addr::new(10, 0, 0, 2),
                Ipv4Addr::new(255, 255, 255, 252)
            ))
        );
    }

    #[test]
    fn ip_cmds() {
        let br = BridgeName::new("arilbox0").unwrap();
        assert_eq!(
            args(&bridge_add_cmd(&br)),
            ["link", "add", "name", "arilbox0", "type", "bridge"]
        );
        assert_eq!(
            args(&bridge_addr_cmd(&br, &cidr("10.88.0.1/24"))),
            ["addr", "replace", "10.88.0.1/24", "dev", "arilbox0"]
        );
        assert_eq!(
            args(&bridge_up_cmd(&br)),
            ["link", "set", "dev", "arilbox0", "up"]
        );
    }

    #[test]
    fn nft_ruleset_text() {
        assert_eq!(
            nft_ruleset("arilbox", &cidr("10.88.0.1/24")),
            "table ip arilbox\n\
             delete table ip arilbox\n\
             table ip arilbox {\n\
             \tchain postrouting {\n\
             \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
             \t\tip saddr 10.88.0.0/24 ip daddr != 10.88.0.0/24 masquerade\n\
             \t}\n\
             }\n"
        );
        let cmd = nft_load_cmd("arilbox", &cidr("10.88.0.1/24"));
        assert_eq!(args(&cmd), ["-f", "-"]);
        assert!(cmd.stdin.is_some());
    }

    #[test]
    fn dnsmasq_args() {
        let a = args(&dnsmasq_cmd(&net(Path::new("/var/lib/arilbox/net"))).unwrap());
        assert_eq!(
            a,
            [
                "--keep-in-foreground",
                "--conf-file=",
                "--log-facility=-",
                "--strict-order",
                "--bind-interfaces",
                "--except-interface=lo",
                "--interface=arilbox0",
                "--listen-address=10.88.0.1",
                "--dhcp-range=10.88.0.2,10.88.0.254,255.255.255.0,1h",
                "--dhcp-no-override",
                "--dhcp-authoritative",
                "--dhcp-leasefile=/var/lib/arilbox/net/arilbox0.leases",
                "--pid-file=/var/lib/arilbox/net/dnsmasq-arilbox0.pid",
            ]
        );
    }

    /// A fake sysfs/procfs tree; removed on drop.
    struct FakeSys(PathBuf);

    impl FakeSys {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("arilbox-test-net-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("proc/sys/net/ipv4")).unwrap();
            std::fs::write(root.join("proc/sys/net/ipv4/ip_forward"), "0\n").unwrap();
            Self(root)
        }
        fn sys(&self) -> Sys {
            Sys::new(&self.0)
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn setup_creates_missing_bridge() {
        let fake = FakeSys::new("create");
        let exec = RecordingExec::new();
        let n = net(&fake.0.join("state"));
        setup(&exec, &fake.sys(), &n).await.unwrap();

        let calls = exec.calls();
        let programs: Vec<_> = calls.iter().map(|c| (c.program, args(c))).collect();
        assert_eq!(programs[0].1[..2], ["link", "add"]);
        assert_eq!(programs[1].1[..2], ["addr", "replace"]);
        assert_eq!(programs[2].1[..2], ["link", "set"]);
        assert_eq!(programs[3].0, Program::Nft);
        assert_eq!(
            std::fs::read_to_string(fake.0.join("proc/sys/net/ipv4/ip_forward")).unwrap(),
            "1"
        );
        assert!(fake.0.join("state").is_dir());
    }

    #[tokio::test]
    async fn setup_reuses_existing_bridge() {
        let fake = FakeSys::new("reuse");
        std::fs::create_dir_all(fake.0.join("sys/class/net/arilbox0/bridge")).unwrap();
        let exec = RecordingExec::new();
        setup(&exec, &fake.sys(), &net(&fake.0.join("state")))
            .await
            .unwrap();
        assert!(exec.calls().iter().all(|c| args(c)[..2] != ["link", "add"]));
    }

    #[tokio::test]
    async fn setup_refuses_non_bridge_interface() {
        let fake = FakeSys::new("notbridge");
        std::fs::create_dir_all(fake.0.join("sys/class/net/arilbox0")).unwrap();
        let exec = RecordingExec::new();
        let err = setup(&exec, &fake.sys(), &net(&fake.0.join("state")))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotABridge(_)));
        assert!(exec.calls().is_empty(), "nothing touched");
    }

    #[tokio::test]
    async fn setup_stops_on_command_failure() {
        let fake = FakeSys::new("fail");
        let exec = RecordingExec::new();
        exec.push_response(Err(ExecError::Failed {
            program: Program::Ip,
            code: Some(2),
            stderr_tail: "RTNETLINK answers: Operation not permitted".into(),
        }));
        assert!(
            setup(&exec, &fake.sys(), &net(&fake.0.join("state")))
                .await
                .is_err()
        );
        assert_eq!(exec.calls().len(), 1);
    }
}
