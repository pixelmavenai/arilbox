//! In-memory [`ContainerBackend`] for API and job tests.

use std::collections::{BTreeMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use arilbox_core::{ContainerName, SnapshotName};
use time::OffsetDateTime;

use crate::backend::ContainerBackend;
use crate::console::{Console, ConsoleInput, TermSize};
use crate::error::{Error, Result};
use crate::types::{ContainerInfo, ContainerState, CreateSpec, ExecOutput, Limits, SnapshotInfo};

#[derive(Debug, Clone)]
struct Entry {
    info: ContainerInfo,
    limits: Limits,
    snapshots: Vec<(SnapshotInfo, Limits)>,
}

#[derive(Debug, Default)]
struct State {
    containers: BTreeMap<ContainerName, Entry>,
    next_pid: u32,
    next_ip: u8,
}

/// In-memory backend. Follows the same state rules as a real backend (start only when
/// stopped, destroy and restore only when stopped, …) without touching the host.
#[derive(Debug, Default)]
pub struct MockBackend {
    state: Mutex<State>,
    failures: Mutex<HashSet<&'static str>>,
    console_sizes: Arc<Mutex<Vec<TermSize>>>,
}

impl MockBackend {
    /// An empty backend.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current limits of a container (the trait has no getter; handy in tests).
    pub fn limits(&self, name: &ContainerName) -> Result<Limits> {
        Ok(self.lock().get(name)?.limits)
    }

    /// Every terminal size a mock console was opened with or resized to, in order.
    pub fn console_sizes(&self) -> Vec<TermSize> {
        self.console_sizes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Makes every later call of operation `op` (`"create"`, `"start"`, `"stop"`, `"kill"`,
    /// `"destroy"`, …) fail with [`Error::Backend`]. `"network"` makes `start` succeed without
    /// assigning an address.
    pub fn fail_on(&self, op: &'static str) {
        self.failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(op);
    }

    fn check(&self, op: &'static str) -> Result<()> {
        let failing = self
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(op);
        if failing {
            Err(Error::Backend(format!("injected failure: {op}")))
        } else {
            Ok(())
        }
    }

    fn lock(&self) -> Guard<'_> {
        Guard(self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

struct Guard<'a>(MutexGuard<'a, State>);

impl Guard<'_> {
    fn get(&self, name: &ContainerName) -> Result<&Entry> {
        self.0
            .containers
            .get(name)
            .ok_or_else(|| Error::NotFound(name.clone()))
    }

    fn get_mut(&mut self, name: &ContainerName) -> Result<&mut Entry> {
        self.0
            .containers
            .get_mut(name)
            .ok_or_else(|| Error::NotFound(name.clone()))
    }

    fn require_state(&self, name: &ContainerName, want: ContainerState) -> Result<()> {
        let state = self.get(name)?.info.state;
        if state == want {
            Ok(())
        } else {
            Err(Error::WrongState {
                name: name.clone(),
                state,
            })
        }
    }

    fn set_stopped(&mut self, name: &ContainerName) -> Result<()> {
        let info = &mut self.get_mut(name)?.info;
        info.state = ContainerState::Stopped;
        info.pid = None;
        info.ipv4.clear();
        info.ipv6.clear();
        info.memory_bytes = 0;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ContainerBackend for MockBackend {
    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        Ok(self
            .lock()
            .0
            .containers
            .values()
            .map(|e| e.info.clone())
            .collect())
    }

    async fn info(&self, name: &ContainerName) -> Result<ContainerInfo> {
        Ok(self.lock().get(name)?.info.clone())
    }

    async fn create(&self, spec: &CreateSpec) -> Result<()> {
        self.check("create")?;
        let mut s = self.lock();
        if s.0.containers.contains_key(&spec.name) {
            return Err(Error::Exists(spec.name.clone()));
        }
        let info = ContainerInfo {
            name: spec.name.clone(),
            state: ContainerState::Stopped,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            pid: None,
            cpu_usage_ns: 0,
            memory_bytes: 0,
            created_at: OffsetDateTime::now_utc(),
            template: spec.template.clone(),
            gpu: spec.gpu,
            app_id: None,
        };
        s.0.containers.insert(
            spec.name.clone(),
            Entry {
                info,
                limits: spec.limits,
                snapshots: Vec::new(),
            },
        );
        Ok(())
    }

    async fn start(&self, name: &ContainerName) -> Result<()> {
        self.check("start")?;
        let no_network = self.check("network").is_err();
        let mut s = self.lock();
        s.require_state(name, ContainerState::Stopped)?;
        s.0.next_pid += 1;
        s.0.next_ip = s.0.next_ip.wrapping_add(1).max(2);
        let (pid, ip) = (1000 + s.0.next_pid, Ipv4Addr::new(10, 88, 0, s.0.next_ip));
        let info = &mut s.get_mut(name)?.info;
        info.state = ContainerState::Running;
        info.pid = Some(pid);
        info.ipv4 = if no_network { Vec::new() } else { vec![ip] };
        info.memory_bytes = 64 * 1024 * 1024;
        Ok(())
    }

    async fn stop(&self, name: &ContainerName, _timeout: Duration) -> Result<()> {
        self.check("stop")?;
        let mut s = self.lock();
        s.require_state(name, ContainerState::Running)?;
        s.set_stopped(name)
    }

    async fn kill(&self, name: &ContainerName) -> Result<()> {
        self.check("kill")?;
        let mut s = self.lock();
        s.require_state(name, ContainerState::Running)?;
        s.set_stopped(name)
    }

    async fn destroy(&self, name: &ContainerName) -> Result<()> {
        self.check("destroy")?;
        let mut s = self.lock();
        s.require_state(name, ContainerState::Stopped)?;
        s.0.containers.remove(name);
        Ok(())
    }

    async fn set_limits(&self, name: &ContainerName, limits: &Limits) -> Result<()> {
        self.lock().get_mut(name)?.limits = *limits;
        Ok(())
    }

    async fn snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()> {
        let mut s = self.lock();
        let entry = s.get_mut(name)?;
        if entry.snapshots.iter().any(|(i, _)| &i.name == snap) {
            return Err(Error::SnapshotExists {
                name: name.clone(),
                snap: snap.clone(),
            });
        }
        let info = SnapshotInfo {
            name: snap.clone(),
            created_at: OffsetDateTime::now_utc(),
        };
        let limits = entry.limits;
        entry.snapshots.push((info, limits));
        Ok(())
    }

    async fn restore(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()> {
        let mut s = self.lock();
        s.require_state(name, ContainerState::Stopped)?;
        let entry = s.get_mut(name)?;
        let (_, limits) = entry
            .snapshots
            .iter()
            .find(|(i, _)| &i.name == snap)
            .ok_or_else(|| Error::SnapshotNotFound {
                name: name.clone(),
                snap: snap.clone(),
            })?;
        entry.limits = *limits;
        Ok(())
    }

    async fn delete_snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()> {
        let mut s = self.lock();
        let entry = s.get_mut(name)?;
        let before = entry.snapshots.len();
        entry.snapshots.retain(|(i, _)| &i.name != snap);
        if entry.snapshots.len() == before {
            return Err(Error::SnapshotNotFound {
                name: name.clone(),
                snap: snap.clone(),
            });
        }
        Ok(())
    }

    async fn list_snapshots(&self, name: &ContainerName) -> Result<Vec<SnapshotInfo>> {
        Ok(self
            .lock()
            .get(name)?
            .snapshots
            .iter()
            .map(|(i, _)| i.clone())
            .collect())
    }

    async fn exec(&self, name: &ContainerName, cmd: &[String]) -> Result<ExecOutput> {
        self.lock().require_state(name, ContainerState::Running)?;
        // Echo the argument vector so tests can see what would have run.
        let mut stdout = cmd.join(" ").into_bytes();
        stdout.push(b'\n');
        Ok(ExecOutput {
            exit_code: 0,
            stdout,
            stderr: Vec::new(),
        })
    }

    /// A loopback terminal: prints a banner, then echoes input (`\r` as `\r\n`). Shutting
    /// down the input ends the output.
    async fn console(&self, name: &ContainerName, size: TermSize) -> Result<Console> {
        self.check("console")?;
        self.lock().require_state(name, ContainerState::Running)?;
        let (user, backend) = tokio::io::duplex(4096);
        let (output, writer) = tokio::io::split(user);
        let mut input = MockConsoleInput {
            writer,
            sizes: self.console_sizes.clone(),
        };
        input.resize(size)?;
        tokio::spawn(echo(backend, format!("mock console: {name}\r\n# ")));
        Ok(Console {
            output: Box::new(output),
            input: Box::new(input),
            session: Box::new(()),
        })
    }
}

async fn echo(mut stream: tokio::io::DuplexStream, banner: String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if stream.write_all(banner.as_bytes()).await.is_err() {
        return;
    }
    let mut buf = [0u8; 1024];
    while let Ok(n @ 1..) = stream.read(&mut buf).await {
        let mut out = Vec::with_capacity(n);
        for &b in &buf[..n] {
            if b == b'\r' {
                out.extend_from_slice(b"\r\n");
            } else {
                out.push(b);
            }
        }
        if stream.write_all(&out).await.is_err() {
            return;
        }
    }
}

struct MockConsoleInput {
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    sizes: Arc<Mutex<Vec<TermSize>>>,
}

impl ConsoleInput for MockConsoleInput {
    fn resize(&mut self, size: TermSize) -> Result<()> {
        self.sizes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(size);
        Ok(())
    }
}

impl tokio::io::AsyncWrite for MockConsoleInput {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NetworkSpec;
    use arilbox_core::config::TemplateRef;
    use arilbox_core::{BridgeName, StorageName};

    fn name(s: &str) -> ContainerName {
        ContainerName::new(s).unwrap()
    }

    fn snap(s: &str) -> SnapshotName {
        SnapshotName::new(s).unwrap()
    }

    fn spec(n: &str) -> CreateSpec {
        CreateSpec {
            name: name(n),
            template: TemplateRef::default(),
            storage: StorageName::new("local-zfs").unwrap(),
            limits: Limits {
                cores: Some(2),
                memory_mb: Some(1024),
                ..Limits::default()
            },
            network: NetworkSpec {
                bridge: BridgeName::new("arilbox0").unwrap(),
                mode: Default::default(),
            },
            gpu: false,
            nesting: false,
            autostart: false,
        }
    }

    const T: Duration = Duration::from_secs(1);

    #[tokio::test]
    async fn lifecycle() {
        let b = MockBackend::new();
        let web = name("web");
        b.create(&spec("web")).await.unwrap();
        assert!(matches!(
            b.create(&spec("web")).await,
            Err(Error::Exists(_))
        ));

        let info = b.info(&web).await.unwrap();
        assert_eq!(info.state, ContainerState::Stopped);
        assert!(info.pid.is_none());

        b.start(&web).await.unwrap();
        let info = b.info(&web).await.unwrap();
        assert_eq!(info.state, ContainerState::Running);
        assert!(info.pid.is_some());
        assert_eq!(info.ipv4.len(), 1);
        assert!(matches!(b.start(&web).await, Err(Error::WrongState { .. })));
        assert!(matches!(
            b.destroy(&web).await,
            Err(Error::WrongState { .. })
        ));

        let out = b.exec(&web, &["echo".into(), "hi".into()]).await.unwrap();
        assert_eq!(out.stdout, b"echo hi\n");

        b.stop(&web, T).await.unwrap();
        assert!(b.info(&web).await.unwrap().ipv4.is_empty());
        assert!(matches!(
            b.stop(&web, T).await,
            Err(Error::WrongState { .. })
        ));
        assert!(matches!(
            b.exec(&web, &[]).await,
            Err(Error::WrongState { .. })
        ));

        b.destroy(&web).await.unwrap();
        assert!(matches!(b.info(&web).await, Err(Error::NotFound(_))));
        assert!(b.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn injected_failures() {
        let b = MockBackend::new();
        b.fail_on("network");
        b.create(&spec("a")).await.unwrap();
        b.start(&name("a")).await.unwrap();
        assert!(b.info(&name("a")).await.unwrap().ipv4.is_empty());
        b.fail_on("create");
        assert!(matches!(b.create(&spec("b")).await, Err(Error::Backend(_))));
    }

    #[tokio::test]
    async fn kill_stops() {
        let b = MockBackend::new();
        b.create(&spec("a")).await.unwrap();
        b.start(&name("a")).await.unwrap();
        b.kill(&name("a")).await.unwrap();
        assert_eq!(
            b.info(&name("a")).await.unwrap().state,
            ContainerState::Stopped
        );
    }

    #[tokio::test]
    async fn unknown_container_is_not_found() {
        let b = MockBackend::new();
        let x = name("missing");
        assert!(matches!(b.info(&x).await, Err(Error::NotFound(_))));
        assert!(matches!(b.start(&x).await, Err(Error::NotFound(_))));
        assert!(matches!(b.destroy(&x).await, Err(Error::NotFound(_))));
        assert!(matches!(
            b.snapshot(&x, &snap("s")).await,
            Err(Error::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn snapshot_restore_roundtrip() {
        let b = MockBackend::new();
        let a = name("a");
        b.create(&spec("a")).await.unwrap();
        let original = b.limits(&a).unwrap();

        b.snapshot(&a, &snap("before")).await.unwrap();
        assert!(matches!(
            b.snapshot(&a, &snap("before")).await,
            Err(Error::SnapshotExists { .. })
        ));

        let changed = Limits {
            cores: Some(8),
            ..original
        };
        b.set_limits(&a, &changed).await.unwrap();
        assert_eq!(b.limits(&a).unwrap(), changed);

        b.start(&a).await.unwrap();
        assert!(matches!(
            b.restore(&a, &snap("before")).await,
            Err(Error::WrongState { .. })
        ));
        b.stop(&a, T).await.unwrap();

        b.restore(&a, &snap("before")).await.unwrap();
        assert_eq!(b.limits(&a).unwrap(), original);
        assert!(matches!(
            b.restore(&a, &snap("nope")).await,
            Err(Error::SnapshotNotFound { .. })
        ));

        let snaps = b.list_snapshots(&a).await.unwrap();
        assert_eq!(snaps.len(), 1);
        b.delete_snapshot(&a, &snap("before")).await.unwrap();
        assert!(matches!(
            b.delete_snapshot(&a, &snap("before")).await,
            Err(Error::SnapshotNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn info_serializes_per_spec() {
        let b = MockBackend::new();
        b.create(&spec("a")).await.unwrap();
        let json = serde_json::to_value(b.info(&name("a")).await.unwrap()).unwrap();
        for field in [
            "name",
            "state",
            "ipv4",
            "ipv6",
            "pid",
            "cpu_usage_ns",
            "memory_bytes",
            "created_at",
            "template",
            "gpu",
            "app_id",
        ] {
            assert!(json.get(field).is_some(), "missing {field}");
        }
        assert_eq!(json["state"], "stopped");
        // RFC 3339 UTC
        assert!(json["created_at"].as_str().unwrap().ends_with('Z'));
    }

    #[tokio::test]
    async fn console_echoes_and_records_sizes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let b = MockBackend::new();
        b.create(&spec("web")).await.unwrap();
        let size = TermSize { rows: 24, cols: 80 };
        assert!(matches!(
            b.console(&name("web"), size).await,
            Err(Error::WrongState { .. })
        ));
        b.start(&name("web")).await.unwrap();
        let mut c = b.console(&name("web"), size).await.unwrap();
        c.input.write_all(b"ls\r").await.unwrap();
        c.input
            .resize(TermSize {
                rows: 50,
                cols: 120,
            })
            .unwrap();
        c.input.shutdown().await.unwrap();
        let mut out = String::new();
        c.output.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, "mock console: web\r\n# ls\r\n");
        assert_eq!(
            b.console_sizes(),
            [
                size,
                TermSize {
                    rows: 50,
                    cols: 120
                }
            ]
        );
    }
}
