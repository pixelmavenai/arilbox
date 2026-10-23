//! [`ContainerBackend`] on top of the `lxc-*` command-line tools.
//!
//! Every command goes through [`arilbox_core::exec`]; argument vectors are built by the pure
//! `*_cmd` functions below and unit-tested. Only containers created by Arilbox are visible:
//! they carry an `arilbox.json` metadata file next to their LXC config ([`META_FILE`]).
//!
//! Snapshots and live limit changes arrive in M4 and return [`Error::Unsupported`] until then.

use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arilbox_core::config::{StoragePool, TemplateRef};
use arilbox_core::exec::{CommandSpec, Exec, ExecError, Program};
use arilbox_core::{BridgeName, ContainerName, DatasetPath, SnapshotName, StorageName};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::backend::ContainerBackend;
use crate::config::{ContainerConfig, MacAddr, Rootfs, render_config, seed_config};
use crate::error::{Error, Result};
use crate::fsutil::write_atomic;
use crate::template::{Image, host_arch, image_for};
use crate::types::{ContainerInfo, ContainerState, CreateSpec, ExecOutput, Limits, SnapshotInfo};

/// Metadata file that marks a container as managed by Arilbox.
pub const META_FILE: &str = "arilbox.json";

/// Image download + unpack can be slow on a cold cache.
const CREATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const DESTROY_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What Arilbox knows about a container beyond the LXC config. Enough to re-render the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// Format version.
    pub version: u32,
    /// Template it was created from.
    pub template: TemplateRef,
    /// Storage pool.
    pub storage: StorageName,
    /// Root filesystem location.
    pub rootfs: Rootfs,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// `eth0` MAC.
    pub hwaddr: MacAddr,
    /// `eth0` bridge.
    pub bridge: BridgeName,
    /// Resource limits.
    pub limits: Limits,
    /// Start on boot.
    pub autostart: bool,
    /// Nesting enabled.
    pub nesting: bool,
    /// GPU shared.
    pub gpu: bool,
    /// Installed catalog app.
    pub app_id: Option<String>,
}

/// LXC backend.
pub struct LxcCli {
    exec: Arc<dyn Exec>,
    lxc_path: PathBuf,
    pools: Vec<StoragePool>,
    work_dir: PathBuf,
}

impl LxcCli {
    /// `lxc_path` is LXC's container directory (`-P`), `pools` the configured storage pools,
    /// `work_dir` a root-only directory for temporary files.
    pub fn new(
        exec: Arc<dyn Exec>,
        lxc_path: PathBuf,
        pools: Vec<StoragePool>,
        work_dir: PathBuf,
    ) -> Self {
        Self {
            exec,
            lxc_path,
            pools,
            work_dir,
        }
    }

    fn container_dir(&self, name: &ContainerName) -> PathBuf {
        self.lxc_path.join(name.as_str())
    }

    fn read_meta(&self, name: &ContainerName) -> Result<Meta> {
        let path = self.container_dir(name).join(META_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(name.clone()));
            }
            Err(e) => return Err(Error::Backend(format!("reading {}: {e}", path.display()))),
        };
        serde_json::from_str(&text)
            .map_err(|e| Error::Backend(format!("parsing {}: {e}", path.display())))
    }

    fn write_files(&self, name: &ContainerName, meta: &Meta, config: &str) -> Result<()> {
        let dir = self.container_dir(name);
        let json = serde_json::to_vec_pretty(meta)
            .map_err(|e| Error::Backend(format!("serializing metadata: {e}")))?;
        write_atomic(&dir.join("config"), config.as_bytes(), 0o640)
            .and_then(|()| write_atomic(&dir.join(META_FILE), &json, 0o640))
            .map_err(|e| Error::Backend(format!("writing config for {name}: {e}")))
    }

    fn pool(&self, name: &StorageName) -> Result<&StoragePool> {
        self.pools
            .iter()
            .find(|p| p.name() == name)
            .ok_or(Error::Unsupported("unknown storage pool"))
    }

    async fn run(&self, spec: CommandSpec) -> Result<arilbox_core::exec::Output> {
        self.exec.run(spec).await.map_err(exec_error)
    }

    /// Writes the seed config for `lxc-create` (so the rootfs is unpacked with the idmap).
    fn write_seed(&self, name: &ContainerName, config: &str) -> Result<PathBuf> {
        use std::io::Write;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.work_dir)
            .map_err(|e| Error::Backend(format!("creating {}: {e}", self.work_dir.display())))?;
        let path = self.work_dir.join(format!("{name}.create.conf"));
        let _ = std::fs::remove_file(&path);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| f.write_all(config.as_bytes()))
            .map_err(|e| Error::Backend(format!("writing {}: {e}", path.display())))?;
        Ok(path)
    }

    /// Removes what a failed or finished create leaves behind. Never fails.
    async fn cleanup_failed_create(&self, name: &ContainerName, rootfs: &Rootfs) {
        if self.container_dir(name).join("config").exists()
            && let Err(e) = self.run(destroy_cmd(&self.lxc_path, name)).await
        {
            tracing::warn!(%name, error = %e, "cleanup: lxc-destroy failed");
        }
        // Remove the container dir if lxc-destroy didn't (only our own files can be there).
        let _ = std::fs::remove_dir_all(self.container_dir(name));
        remove_pool_dir(rootfs);
    }
}

/// Removes `<pool>/containers/<name>` for dir pools once the rootfs data is gone (an empty
/// `rootfs` dir, e.g. after a failed create, is removed too). Uses `remove_dir`, never
/// recursive, so leftover data is reported rather than deleted.
fn remove_pool_dir(rootfs: &Rootfs) {
    let Rootfs::Dir { path } = rootfs else {
        return;
    };
    // Fails harmlessly if lxc-destroy already removed it or it still holds data.
    let _ = std::fs::remove_dir(path);
    if let Some(parent) = path.parent()
        && parent.exists()
        && let Err(e) = std::fs::remove_dir(parent)
    {
        tracing::warn!(dir = %parent.display(), error = %e, "could not remove container storage dir");
    }
}

fn exec_error(e: ExecError) -> Error {
    // Full details (stderr) go to the log; API clients only see a stable code.
    tracing::error!(error = %e, "lxc command failed");
    Error::Backend(e.to_string())
}

/// Rootfs location for a new container in `pool`.
pub fn rootfs_for(pool: &StoragePool, name: &ContainerName) -> Result<Rootfs> {
    Ok(match pool {
        StoragePool::Dir { path, .. } => Rootfs::Dir {
            path: path.join("containers").join(name.as_str()).join("rootfs"),
        },
        StoragePool::Zfs { dataset, .. } => Rootfs::Zfs {
            dataset: DatasetPath::new(format!("{dataset}/containers/{name}"))
                .map_err(|e| Error::Backend(e.to_string()))?,
        },
    })
}

// ---- command builders (pure) ----------------------------------------------------------------

fn base_args(lxc_path: &Path, name: &ContainerName) -> Vec<std::ffi::OsString> {
    vec![
        "-P".into(),
        lxc_path.as_os_str().into(),
        "-n".into(),
        name.as_os_str().into(),
    ]
}

/// `lxc-create … -t download -- --dist … --release … --arch …`
pub fn create_cmd(
    lxc_path: &Path,
    name: &ContainerName,
    seed: &Path,
    rootfs: &Rootfs,
    image: Image,
    arch: &str,
) -> CommandSpec {
    let mut args = base_args(lxc_path, name);
    args.extend([
        "-f".into(),
        seed.as_os_str().into(),
        "-t".into(),
        "download".into(),
    ]);
    match rootfs {
        Rootfs::Dir { path } => {
            args.extend([
                "-B".into(),
                "dir".into(),
                "--dir".into(),
                path.as_os_str().into(),
            ]);
        }
        Rootfs::Zfs { dataset } => {
            // LXC creates <zfsroot>/<name>.
            let root = dataset
                .as_str()
                .rsplit_once('/')
                .map_or(dataset.as_str(), |(parent, _)| parent);
            args.extend(["-B".into(), "zfs".into(), "--zfsroot".into(), root.into()]);
        }
    }
    args.extend(
        [
            "--",
            "--dist",
            image.dist,
            "--release",
            image.release,
            "--arch",
            arch,
        ]
        .map(Into::into),
    );
    CommandSpec::new(Program::LxcCreate, args).timeout(CREATE_TIMEOUT)
}

/// `lxc-start` (daemonizes once the container is started).
pub fn start_cmd(lxc_path: &Path, name: &ContainerName) -> CommandSpec {
    CommandSpec::new(Program::LxcStart, base_args(lxc_path, name)).timeout(START_TIMEOUT)
}

/// `lxc-stop -t <secs>`: clean shutdown, killed after the timeout.
pub fn stop_cmd(lxc_path: &Path, name: &ContainerName, timeout: Duration) -> CommandSpec {
    let mut args = base_args(lxc_path, name);
    args.extend(["-t".into(), timeout.as_secs().to_string().into()]);
    CommandSpec::new(Program::LxcStop, args).timeout(timeout + Duration::from_secs(15))
}

/// `lxc-stop -k`
pub fn kill_cmd(lxc_path: &Path, name: &ContainerName) -> CommandSpec {
    let mut args = base_args(lxc_path, name);
    args.push("-k".into());
    CommandSpec::new(Program::LxcStop, args)
}

/// `lxc-destroy`
pub fn destroy_cmd(lxc_path: &Path, name: &ContainerName) -> CommandSpec {
    CommandSpec::new(Program::LxcDestroy, base_args(lxc_path, name)).timeout(DESTROY_TIMEOUT)
}

/// `lxc-info -H` (raw numbers).
pub fn info_cmd(lxc_path: &Path, name: &ContainerName) -> CommandSpec {
    let mut args = base_args(lxc_path, name);
    args.push("-H".into());
    CommandSpec::new(Program::LxcInfo, args)
}

/// `lxc-attach --clear-env -- <cmd…>`
pub fn attach_cmd(lxc_path: &Path, name: &ContainerName, cmd: &[String]) -> CommandSpec {
    let mut args = base_args(lxc_path, name);
    args.extend(["--clear-env".into(), "--".into()]);
    args.extend(cmd.iter().map(Into::into));
    CommandSpec::new(Program::LxcAttach, args).timeout(Duration::from_secs(120))
}

// ---- lxc-info parsing -----------------------------------------------------------------------

/// Runtime fields parsed from `lxc-info -H`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// State.
    pub state: Option<ContainerState>,
    /// Init PID.
    pub pid: Option<u32>,
    /// Addresses.
    pub ips: Vec<IpAddr>,
    /// CPU time.
    pub cpu_ns: u64,
    /// Memory.
    pub memory_bytes: u64,
}

/// Parses `lxc-info -H` output. Unknown lines are ignored.
pub fn parse_info(text: &str) -> RuntimeInfo {
    let mut info = RuntimeInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "State" => info.state = Some(parse_state(value)),
            "PID" => info.pid = value.parse().ok(),
            "IP" => info.ips.extend(value.parse::<IpAddr>().ok()),
            "CPU use" => info.cpu_ns = value.parse().unwrap_or(0),
            "Memory use" => info.memory_bytes = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    info
}

fn parse_state(s: &str) -> ContainerState {
    match s {
        "STOPPED" => ContainerState::Stopped,
        "STARTING" => ContainerState::Starting,
        "RUNNING" | "THAWED" => ContainerState::Running,
        "STOPPING" | "ABORTING" => ContainerState::Stopping,
        "FROZEN" | "FREEZING" => ContainerState::Frozen,
        _ => ContainerState::Error,
    }
}

// ---- backend --------------------------------------------------------------------------------

#[async_trait::async_trait]
impl ContainerBackend for LxcCli {
    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        let entries = match std::fs::read_dir(&self.lxc_path) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Backend(format!("reading lxc path: {e}"))),
        };
        let mut names: Vec<ContainerName> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join(META_FILE).is_file())
            // Directories with invalid names are not ours, whatever they contain.
            .filter_map(|e| ContainerName::new(e.file_name().to_string_lossy()).ok())
            .collect();
        names.sort();

        let mut out = Vec::with_capacity(names.len());
        for name in &names {
            match self.info(name).await {
                Ok(info) => out.push(info),
                // Deleted between listing and lookup.
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn info(&self, name: &ContainerName) -> Result<ContainerInfo> {
        let meta = self.read_meta(name)?;
        let output = self.run(info_cmd(&self.lxc_path, name)).await?;
        let rt = parse_info(&String::from_utf8_lossy(&output.stdout));
        let running = rt.state == Some(ContainerState::Running);
        Ok(ContainerInfo {
            name: name.clone(),
            state: rt.state.unwrap_or(ContainerState::Error),
            ipv4: rt
                .ips
                .iter()
                .filter_map(|ip| match ip {
                    IpAddr::V4(v4) => Some(*v4),
                    IpAddr::V6(_) => None,
                })
                .collect(),
            ipv6: rt
                .ips
                .iter()
                .filter_map(|ip| match ip {
                    IpAddr::V6(v6) => Some(*v6),
                    IpAddr::V4(_) => None,
                })
                .collect(),
            pid: rt.pid.filter(|_| running),
            cpu_usage_ns: rt.cpu_ns,
            memory_bytes: rt.memory_bytes,
            created_at: meta.created_at,
            template: meta.template,
            gpu: meta.gpu,
            app_id: meta.app_id,
        })
    }

    async fn create(&self, spec: &CreateSpec) -> Result<()> {
        let name = &spec.name;
        if self.container_dir(name).exists() {
            return Err(Error::Exists(name.clone()));
        }
        if spec.gpu {
            return Err(Error::Unsupported("GPU sharing"));
        }
        let image = image_for(&spec.template).ok_or(Error::Unsupported("template"))?;
        let arch = host_arch().ok_or(Error::Unsupported("host architecture"))?;
        let rootfs = rootfs_for(self.pool(&spec.storage)?, name)?;
        let hwaddr = MacAddr::random()
            .map_err(|e| Error::Backend(format!("generating MAC address: {e}")))?;

        let meta = Meta {
            version: 1,
            template: spec.template.clone(),
            storage: spec.storage.clone(),
            rootfs: rootfs.clone(),
            created_at: OffsetDateTime::now_utc(),
            hwaddr,
            bridge: spec.network.bridge.clone(),
            limits: spec.limits,
            autostart: spec.autostart,
            nesting: spec.nesting,
            gpu: spec.gpu,
            app_id: None,
        };
        let config = render_config(&config_for(name, arch, &meta))?;

        if let Rootfs::Dir { path } = &rootfs {
            // lxc-create does not create a `--dir` rootfs itself; it chowns it to the mapped
            // root and unpacks into it. 0755: the mapped root must be able to traverse to it.
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(path)
                .map_err(|e| Error::Backend(format!("creating {}: {e}", path.display())))?;
        }

        let seed = self.write_seed(name, &seed_config(&config))?;
        let result = async {
            self.run(create_cmd(
                &self.lxc_path,
                name,
                &seed,
                &rootfs,
                image,
                arch,
            ))
            .await?;
            // Replace whatever lxc-create and the template wrote with our config.
            self.write_files(name, &meta, &config)
        }
        .await;
        let _ = std::fs::remove_file(&seed);

        if result.is_err() {
            self.cleanup_failed_create(name, &rootfs).await;
        }
        result
    }

    async fn start(&self, name: &ContainerName) -> Result<()> {
        let info = self.info(name).await?;
        if info.state != ContainerState::Stopped {
            return Err(Error::WrongState {
                name: name.clone(),
                state: info.state,
            });
        }
        self.run(start_cmd(&self.lxc_path, name)).await.map(drop)
    }

    async fn stop(&self, name: &ContainerName, timeout: Duration) -> Result<()> {
        let info = self.info(name).await?;
        if info.state != ContainerState::Running {
            return Err(Error::WrongState {
                name: name.clone(),
                state: info.state,
            });
        }
        self.run(stop_cmd(&self.lxc_path, name, timeout))
            .await
            .map(drop)
    }

    async fn kill(&self, name: &ContainerName) -> Result<()> {
        let info = self.info(name).await?;
        if info.state == ContainerState::Stopped {
            return Err(Error::WrongState {
                name: name.clone(),
                state: info.state,
            });
        }
        self.run(kill_cmd(&self.lxc_path, name)).await.map(drop)
    }

    async fn destroy(&self, name: &ContainerName) -> Result<()> {
        let meta = self.read_meta(name)?;
        let info = self.info(name).await?;
        if info.state != ContainerState::Stopped {
            return Err(Error::WrongState {
                name: name.clone(),
                state: info.state,
            });
        }
        self.run(destroy_cmd(&self.lxc_path, name)).await?;
        remove_pool_dir(&meta.rootfs);
        Ok(())
    }

    async fn set_limits(&self, _name: &ContainerName, _limits: &Limits) -> Result<()> {
        Err(Error::Unsupported("changing limits (M4)"))
    }

    async fn snapshot(&self, _name: &ContainerName, _snap: &SnapshotName) -> Result<()> {
        Err(Error::Unsupported("snapshots (M4)"))
    }

    async fn restore(&self, _name: &ContainerName, _snap: &SnapshotName) -> Result<()> {
        Err(Error::Unsupported("snapshots (M4)"))
    }

    async fn delete_snapshot(&self, _name: &ContainerName, _snap: &SnapshotName) -> Result<()> {
        Err(Error::Unsupported("snapshots (M4)"))
    }

    async fn list_snapshots(&self, _name: &ContainerName) -> Result<Vec<SnapshotInfo>> {
        Err(Error::Unsupported("snapshots (M4)"))
    }

    async fn exec(&self, name: &ContainerName, cmd: &[String]) -> Result<ExecOutput> {
        let info = self.info(name).await?;
        if info.state != ContainerState::Running {
            return Err(Error::WrongState {
                name: name.clone(),
                state: info.state,
            });
        }
        match self.exec.run(attach_cmd(&self.lxc_path, name, cmd)).await {
            Ok(out) => Ok(ExecOutput {
                exit_code: 0,
                stdout: out.stdout,
                stderr: out.stderr,
            }),
            // lxc-attach exits with the command's exit code. Only the stderr tail survives.
            Err(ExecError::Failed {
                code: Some(code),
                stderr_tail,
                ..
            }) => Ok(ExecOutput {
                exit_code: code,
                stdout: Vec::new(),
                stderr: stderr_tail.into_bytes(),
            }),
            Err(e) => Err(exec_error(e)),
        }
    }
}

/// The config inputs for a container from its metadata.
fn config_for(name: &ContainerName, arch: &'static str, meta: &Meta) -> ContainerConfig {
    ContainerConfig {
        name: name.clone(),
        arch,
        rootfs: meta.rootfs.clone(),
        limits: meta.limits,
        autostart: meta.autostart,
        nesting: meta.nesting,
        bridge: meta.bridge.clone(),
        hwaddr: meta.hwaddr,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use arilbox_core::exec::{Output, RecordingExec};

    use super::*;
    use crate::types::NetworkSpec;

    const P: &str = "/var/lib/lxc";

    fn name(s: &str) -> ContainerName {
        ContainerName::new(s).unwrap()
    }

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn create_cmd_dir() {
        let rootfs = Rootfs::Dir {
            path: "/srv/pool/containers/web/rootfs".into(),
        };
        let spec = create_cmd(
            Path::new(P),
            &name("web"),
            Path::new("/var/lib/arilbox/tmp/web.create.conf"),
            &rootfs,
            Image {
                dist: "debian",
                release: "trixie",
            },
            "amd64",
        );
        assert_eq!(spec.program, Program::LxcCreate);
        assert_eq!(spec.timeout, CREATE_TIMEOUT);
        assert_eq!(
            args(&spec),
            [
                "-P",
                P,
                "-n",
                "web",
                "-f",
                "/var/lib/arilbox/tmp/web.create.conf",
                "-t",
                "download",
                "-B",
                "dir",
                "--dir",
                "/srv/pool/containers/web/rootfs",
                "--",
                "--dist",
                "debian",
                "--release",
                "trixie",
                "--arch",
                "amd64"
            ]
        );
    }

    #[test]
    fn create_cmd_zfs_uses_parent_as_zfsroot() {
        let rootfs = Rootfs::Zfs {
            dataset: DatasetPath::new("tank/arilbox/containers/web").unwrap(),
        };
        let a = args(&create_cmd(
            Path::new(P),
            &name("web"),
            Path::new("/seed"),
            &rootfs,
            Image {
                dist: "alpine",
                release: "edge",
            },
            "amd64",
        ));
        let i = a.iter().position(|x| x == "--zfsroot").unwrap();
        assert_eq!(a[i - 1], "zfs");
        assert_eq!(a[i + 1], "tank/arilbox/containers");
    }

    #[test]
    fn simple_cmds() {
        let n = name("web");
        let p = Path::new(P);
        assert_eq!(args(&start_cmd(p, &n)), ["-P", P, "-n", "web"]);
        let stop = stop_cmd(p, &n, Duration::from_secs(30));
        assert_eq!(args(&stop), ["-P", P, "-n", "web", "-t", "30"]);
        assert_eq!(stop.timeout, Duration::from_secs(45));
        assert_eq!(args(&kill_cmd(p, &n)), ["-P", P, "-n", "web", "-k"]);
        assert_eq!(destroy_cmd(p, &n).program, Program::LxcDestroy);
        assert_eq!(args(&info_cmd(p, &n)), ["-P", P, "-n", "web", "-H"]);
        assert_eq!(
            args(&attach_cmd(
                p,
                &n,
                &["ping".into(), "-c1".into(), "a;b".into()]
            )),
            [
                "-P",
                P,
                "-n",
                "web",
                "--clear-env",
                "--",
                "ping",
                "-c1",
                "a;b"
            ]
        );
    }

    #[test]
    fn parses_running_info() {
        let text = "Name:           web\nState:          RUNNING\nPID:            4242\n\
                    IP:             10.88.0.17\nIP:             fd42::17\n\
                    CPU use:        1234567890\nBlkIO use:      0\nMemory use:     52428800\n\
                    KMem use:       0\nLink:           vethABC\n TX bytes:      10\n";
        assert_eq!(
            parse_info(text),
            RuntimeInfo {
                state: Some(ContainerState::Running),
                pid: Some(4242),
                ips: vec!["10.88.0.17".parse().unwrap(), "fd42::17".parse().unwrap()],
                cpu_ns: 1_234_567_890,
                memory_bytes: 52_428_800,
            }
        );
    }

    #[test]
    fn parses_stopped_and_odd_states() {
        let info = parse_info("Name: web\nState: STOPPED\n");
        assert_eq!(info.state, Some(ContainerState::Stopped));
        assert!(info.ips.is_empty() && info.pid.is_none());
        assert_eq!(parse_state("THAWED"), ContainerState::Running);
        assert_eq!(parse_state("ABORTING"), ContainerState::Stopping);
        assert_eq!(parse_state("???"), ContainerState::Error);
    }

    /// A temp tree with an lxc path, a dir pool and a work dir; removed on drop.
    struct Fixture {
        root: PathBuf,
        exec: Arc<RecordingExec>,
        backend: LxcCli,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("arilbox-test-lxccli-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("lxc")).unwrap();
            let exec = Arc::new(RecordingExec::new());
            let backend = LxcCli::new(
                exec.clone(),
                root.join("lxc"),
                vec![StoragePool::Dir {
                    name: StorageName::new("local-dir").unwrap(),
                    path: root.join("pool"),
                }],
                root.join("work"),
            );
            Self {
                root,
                exec,
                backend,
            }
        }

        fn spec(&self, n: &str) -> CreateSpec {
            CreateSpec {
                name: name(n),
                template: TemplateRef::default(),
                storage: StorageName::new("local-dir").unwrap(),
                limits: Limits::default(),
                network: NetworkSpec {
                    bridge: BridgeName::new("arilbox0").unwrap(),
                    mode: Default::default(),
                },
                gpu: false,
                nesting: false,
                autostart: false,
            }
        }

        /// Pretends to be lxc-create: makes the container dir with a config.
        fn fake_lxc_create(&self, n: &str) {
            let dir = self.root.join("lxc").join(n);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config"), "lxc.include = upstream\n").unwrap();
        }

        fn info_output(state: &str) -> Output {
            Output {
                stdout: format!("Name: x\nState: {state}\n").into_bytes(),
                stderr: Vec::new(),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn create_runs_lxc_create_with_seed_and_cleans_up_on_failure() {
        let f = Fixture::new("createfail");
        f.exec.push_response(Err(ExecError::Failed {
            program: Program::LxcCreate,
            code: Some(1),
            stderr_tail: "download failed".into(),
        }));
        let err = f.backend.create(&f.spec("web")).await.unwrap_err();
        assert!(matches!(err, Error::Backend(_)));

        let calls = f.exec.calls();
        assert_eq!(calls[0].program, Program::LxcCreate);
        let a = args(&calls[0]);
        let seed = &a[a.iter().position(|x| x == "-f").unwrap() + 1];
        assert!(seed.ends_with("work/web.create.conf"), "{seed}");
        // The rootfs must come from --dir, not the seed (LXC would expect it to exist).
        let i = a.iter().position(|x| x == "--dir").unwrap();
        assert!(a[i + 1].ends_with("pool/containers/web/rootfs"), "{a:?}");
        // The rootfs dir existed for lxc-create; seed, rootfs and pool dir are all removed,
        // nothing is left in the lxc path.
        assert!(!Path::new(seed).exists());
        assert!(!f.root.join("pool/containers/web").exists());
        assert!(!f.root.join("lxc/web").exists());
    }

    #[tokio::test]
    async fn rootfs_dir_exists_when_lxc_create_runs() {
        /// Records whether the `--dir` path existed when lxc-create was called, then fails.
        struct CheckDir(std::sync::Mutex<Option<bool>>);

        #[async_trait::async_trait]
        impl Exec for CheckDir {
            async fn run(&self, spec: CommandSpec) -> Result<Output, ExecError> {
                if spec.program == Program::LxcCreate {
                    let a = args(&spec);
                    let dir = &a[a.iter().position(|x| x == "--dir").unwrap() + 1];
                    *self.0.lock().unwrap() = Some(Path::new(dir).is_dir());
                }
                Err(ExecError::Failed {
                    program: spec.program,
                    code: Some(1),
                    stderr_tail: String::new(),
                })
            }
        }

        let f = Fixture::new("rootfsdir");
        let check = Arc::new(CheckDir(std::sync::Mutex::new(None)));
        let backend = LxcCli::new(
            check.clone(),
            f.root.join("lxc"),
            vec![StoragePool::Dir {
                name: StorageName::new("local-dir").unwrap(),
                path: f.root.join("pool"),
            }],
            f.root.join("work"),
        );
        assert!(backend.create(&f.spec("web")).await.is_err());
        assert_eq!(
            *check.0.lock().unwrap(),
            Some(true),
            "lxc-create needs an existing --dir"
        );
        assert!(
            !f.root.join("pool/containers/web").exists(),
            "cleaned up after failure"
        );
    }

    #[tokio::test]
    async fn create_rejects_unsupported_inputs_before_running_anything() {
        let f = Fixture::new("unsupported");
        let mut gpu = f.spec("a");
        gpu.gpu = true;
        let mut tpl = f.spec("b");
        tpl.template.release = "12".into();
        let mut pool = f.spec("c");
        pool.storage = StorageName::new("nope").unwrap();
        for spec in [gpu, tpl, pool] {
            assert!(matches!(
                f.backend.create(&spec).await,
                Err(Error::Unsupported(_))
            ));
        }
        assert!(f.exec.calls().is_empty());
    }

    #[tokio::test]
    async fn only_managed_containers_are_listed() {
        let f = Fixture::new("list");
        // A foreign container: config but no arilbox.json.
        f.fake_lxc_create("theirs");
        // A managed one.
        let meta = Meta {
            version: 1,
            template: TemplateRef::default(),
            storage: StorageName::new("local-dir").unwrap(),
            rootfs: Rootfs::Dir {
                path: f.root.join("pool/containers/ours/rootfs"),
            },
            created_at: OffsetDateTime::UNIX_EPOCH,
            hwaddr: MacAddr([0, 0x16, 0x3e, 1, 2, 3]),
            bridge: BridgeName::new("arilbox0").unwrap(),
            limits: Limits::default(),
            autostart: false,
            nesting: false,
            gpu: false,
            app_id: None,
        };
        std::fs::create_dir_all(f.root.join("lxc/ours")).unwrap();
        f.backend.write_files(&name("ours"), &meta, "x").unwrap();
        f.exec.push_response(Ok(Fixture::info_output("RUNNING")));

        let list = f.backend.list().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name.as_str(), "ours");
        assert_eq!(list[0].state, ContainerState::Running);
        // Foreign containers are not found either.
        assert!(matches!(
            f.backend.info(&name("theirs")).await,
            Err(Error::NotFound(_))
        ));
        // No command ever named the foreign container.
        for call in f.exec.calls() {
            assert!(!call.args.contains(&OsString::from("theirs")));
        }
    }

    #[tokio::test]
    async fn state_checks_happen_before_commands() {
        let f = Fixture::new("state");
        std::fs::create_dir_all(f.root.join("lxc/web")).unwrap();
        let meta: Meta = serde_json::from_value(serde_json::json!({
            "version": 1,
            "template": { "distro": "debian", "release": "13" },
            "storage": "local-dir",
            "rootfs": { "type": "dir", "path": "/nonexistent/containers/web/rootfs" },
            "created_at": "2026-10-23T00:00:00Z",
            "hwaddr": "00:16:3e:00:00:01",
            "bridge": "arilbox0",
            "limits": {},
            "autostart": false,
            "nesting": false,
            "gpu": false,
            "app_id": null
        }))
        .unwrap();
        f.backend.write_files(&name("web"), &meta, "x").unwrap();

        // Running: start and destroy refuse without running lxc-start / lxc-destroy.
        f.exec.push_response(Ok(Fixture::info_output("RUNNING")));
        assert!(matches!(
            f.backend.start(&name("web")).await,
            Err(Error::WrongState { .. })
        ));
        f.exec.push_response(Ok(Fixture::info_output("RUNNING")));
        assert!(matches!(
            f.backend.destroy(&name("web")).await,
            Err(Error::WrongState { .. })
        ));
        assert!(f.exec.calls().iter().all(|c| c.program == Program::LxcInfo));

        // Stopped: start runs lxc-start.
        f.exec.push_response(Ok(Fixture::info_output("STOPPED")));
        f.backend.start(&name("web")).await.unwrap();
        assert_eq!(f.exec.calls().last().unwrap().program, Program::LxcStart);
    }
}
