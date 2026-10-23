//! The only allowed way to run external programs.
//!
//! Rules (see `.claude/skills/run-command-safely`): no shell, allow-listed programs resolved to
//! absolute paths at start-up, arguments built from validated newtypes, a clean environment,
//! a timeout on every call, bounded output and typed errors. Nothing else in the codebase may
//! call `Command::new`.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// `PATH` given to every child process, also the search path for [`ProgramPaths::resolve`].
pub const SAFE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
/// Default timeout when a caller doesn't choose one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum bytes kept from each of stdout and stderr.
pub const MAX_OUTPUT: usize = 1024 * 1024;
/// Maximum bytes of stderr kept in [`ExecError::Failed`].
pub const STDERR_TAIL: usize = 2048;

/// Programs Arilbox is allowed to run. Adding one needs a justification in the commit message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Program {
    /// `lxc-create`
    LxcCreate,
    /// `lxc-start`
    LxcStart,
    /// `lxc-stop`
    LxcStop,
    /// `lxc-info`
    LxcInfo,
    /// `lxc-ls`
    LxcLs,
    /// `lxc-snapshot`
    LxcSnapshot,
    /// `lxc-attach`
    LxcAttach,
    /// `lxc-destroy`
    LxcDestroy,
    /// `lxc-cgroup`
    LxcCgroup,
    /// `zfs`
    Zfs,
    /// `zpool`
    Zpool,
    /// `ip`
    Ip,
    /// `nft`
    Nft,
    /// `nvidia-smi`
    NvidiaSmi,
    /// `dnsmasq`
    Dnsmasq,
}

impl Program {
    /// Executable file name.
    pub fn binary(self) -> &'static str {
        match self {
            Program::LxcCreate => "lxc-create",
            Program::LxcStart => "lxc-start",
            Program::LxcStop => "lxc-stop",
            Program::LxcInfo => "lxc-info",
            Program::LxcLs => "lxc-ls",
            Program::LxcSnapshot => "lxc-snapshot",
            Program::LxcAttach => "lxc-attach",
            Program::LxcDestroy => "lxc-destroy",
            Program::LxcCgroup => "lxc-cgroup",
            Program::Zfs => "zfs",
            Program::Zpool => "zpool",
            Program::Ip => "ip",
            Program::Nft => "nft",
            Program::NvidiaSmi => "nvidia-smi",
            Program::Dnsmasq => "dnsmasq",
        }
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.binary())
    }
}

/// Errors from running a program. Callers map these to their own crate errors; `stderr_tail`
/// goes to logs, never to API clients.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// The program was not found during resolution, or was not resolved at start-up.
    #[error("program {program} not found")]
    NotFound {
        /// The program.
        program: Program,
    },
    /// Spawning failed.
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        /// The program.
        program: Program,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// I/O with the child failed.
    #[error("i/o error talking to {program}: {source}")]
    Io {
        /// The program.
        program: Program,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The program ran longer than its timeout and was killed.
    #[error("{program} timed out after {timeout:?}")]
    Timeout {
        /// The program.
        program: Program,
        /// The timeout that was exceeded.
        timeout: Duration,
    },
    /// The program exited unsuccessfully.
    #[error("{program} failed (exit code {code:?}): {stderr_tail}")]
    Failed {
        /// The program.
        program: Program,
        /// Exit code, `None` if killed by a signal.
        code: Option<i32>,
        /// Last [`STDERR_TAIL`] bytes of stderr, lossily decoded.
        stderr_tail: String,
    },
}

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Allow-listed program.
    pub program: Program,
    /// One element per argument. Values from users must come from validated newtypes.
    pub args: Vec<OsString>,
    /// Kill the program after this long.
    pub timeout: Duration,
    /// Bytes written to stdin, which is then closed. Use this (not argv) for secrets.
    pub stdin: Option<Vec<u8>>,
    /// Extra environment variables on top of `PATH` and `LC_ALL`.
    pub env: Vec<(OsString, OsString)>,
}

impl CommandSpec {
    /// A spec with [`DEFAULT_TIMEOUT`], no stdin and no extra environment.
    pub fn new<I, A>(program: Program, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        Self {
            program,
            args: args.into_iter().map(Into::into).collect(),
            timeout: DEFAULT_TIMEOUT,
            stdin: None,
            env: Vec::new(),
        }
    }

    /// Sets the timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets stdin contents.
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    /// Adds an environment variable.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Captured output of a successful run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// Stdout, at most [`MAX_OUTPUT`] bytes.
    pub stdout: Vec<u8>,
    /// Stderr, at most [`MAX_OUTPUT`] bytes.
    pub stderr: Vec<u8>,
}

/// Runs commands. Real in production, [`RecordingExec`] in tests.
#[async_trait::async_trait]
pub trait Exec: Send + Sync {
    /// Runs `spec` to completion. Non-zero exit is [`ExecError::Failed`].
    async fn run(&self, spec: CommandSpec) -> Result<Output, ExecError>;
}

/// Absolute paths of allow-listed programs, resolved once at start-up.
#[derive(Debug, Clone, Default)]
pub struct ProgramPaths(HashMap<Program, PathBuf>);

impl ProgramPaths {
    /// Resolves each program in [`SAFE_PATH`]. Fails on the first missing one.
    pub fn resolve(programs: &[Program]) -> Result<Self, ExecError> {
        let dirs: Vec<&Path> = SAFE_PATH.split(':').map(Path::new).collect();
        Self::resolve_in(&dirs, programs)
    }

    /// Resolves each program in `dirs`, in order.
    pub fn resolve_in(dirs: &[&Path], programs: &[Program]) -> Result<Self, ExecError> {
        let mut map = HashMap::new();
        for &program in programs {
            let path = dirs
                .iter()
                .map(|d| d.join(program.binary()))
                .find(|p| p.is_file())
                .ok_or(ExecError::NotFound { program })?;
            map.insert(program, path);
        }
        Ok(Self(map))
    }

    /// Uses explicit paths. Intended for tests.
    pub fn from_map(map: HashMap<Program, PathBuf>) -> Self {
        Self(map)
    }

    /// Path of a resolved program.
    pub fn get(&self, program: Program) -> Option<&Path> {
        self.0.get(&program).map(PathBuf::as_path)
    }
}

/// Spawns real processes following the rules in the module docs.
#[derive(Debug, Clone)]
pub struct RealExec {
    paths: ProgramPaths,
}

impl RealExec {
    /// Uses `paths` resolved at start-up.
    pub fn new(paths: ProgramPaths) -> Self {
        Self { paths }
    }

    /// A command for `spec` with the safety rules applied (clean env, no shell, kill on drop).
    fn command(&self, spec: &CommandSpec) -> Result<tokio::process::Command, ExecError> {
        let program = spec.program;
        let path = self
            .paths
            .get(program)
            .ok_or(ExecError::NotFound { program })?;
        tracing::debug!(%program, args = ?spec.args, "exec");
        let mut cmd = tokio::process::Command::new(path);
        cmd.args(&spec.args)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .env("LC_ALL", "C")
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true);
        Ok(cmd)
    }

    /// Starts a long-running service (e.g. `dnsmasq` in the foreground) and returns the child
    /// for the caller to supervise. Same rules as [`Exec::run`] except there is no timeout and
    /// no stdin; stdout is discarded and stderr goes to the daemon's stderr (the journal). The
    /// child is killed when the handle is dropped.
    pub fn spawn_service(&self, spec: &CommandSpec) -> Result<tokio::process::Child, ExecError> {
        let program = spec.program;
        self.command(spec)?
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| ExecError::Spawn { program, source })
    }
}

#[async_trait::async_trait]
impl Exec for RealExec {
    async fn run(&self, spec: CommandSpec) -> Result<Output, ExecError> {
        let program = spec.program;
        let mut child = self
            .command(&spec)?
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| ExecError::Spawn { program, source })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let io = |source| ExecError::Io { program, source };

        let work = async {
            let write = async {
                if let (Some(mut pipe), Some(data)) = (stdin, spec.stdin.as_deref()) {
                    match pipe.write_all(data).await {
                        // The child may exit without reading stdin; that is its business.
                        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                        other => other?,
                    }
                }
                Ok::<_, std::io::Error>(())
            };
            let (w, out, err) = tokio::join!(write, read_bounded(stdout), read_bounded(stderr));
            w.map_err(io)?;
            let (out, err) = (out.map_err(io)?, err.map_err(io)?);
            let status = child.wait().await.map_err(io)?;
            Ok::<_, ExecError>((status, out, err))
        };

        let (status, stdout, stderr) = match tokio::time::timeout(spec.timeout, work).await {
            Ok(result) => result?,
            Err(_) => {
                // Kill and reap; the child may already be gone.
                let _ = child.kill().await;
                return Err(ExecError::Timeout {
                    program,
                    timeout: spec.timeout,
                });
            }
        };
        finish(program, status, stdout, stderr)
    }
}

fn finish(
    program: Program,
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<Output, ExecError> {
    if status.success() {
        return Ok(Output { stdout, stderr });
    }
    let tail = &stderr[stderr.len().saturating_sub(STDERR_TAIL)..];
    Err(ExecError::Failed {
        program,
        code: status.code(),
        stderr_tail: String::from_utf8_lossy(tail).trim().to_owned(),
    })
}

/// Reads everything from `reader`, keeping only the first [`MAX_OUTPUT`] bytes. The rest is
/// drained so the child never blocks on a full pipe.
async fn read_bounded<R: AsyncRead + Unpin>(reader: Option<R>) -> std::io::Result<Vec<u8>> {
    let Some(mut reader) = reader else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(kept);
        }
        let room = MAX_OUTPUT - kept.len();
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
}

/// Test double: records every spec and returns scripted results in order (empty success once
/// the script runs out).
#[derive(Debug, Default)]
pub struct RecordingExec {
    calls: Mutex<Vec<CommandSpec>>,
    responses: Mutex<VecDeque<Result<Output, ExecError>>>,
}

impl RecordingExec {
    /// An exec with no scripted responses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the result for the next call.
    pub fn push_response(&self, response: Result<Output, ExecError>) {
        lock(&self.responses).push_back(response);
    }

    /// All specs run so far.
    pub fn calls(&self) -> Vec<CommandSpec> {
        lock(&self.calls).clone()
    }
}

#[async_trait::async_trait]
impl Exec for RecordingExec {
    async fn run(&self, spec: CommandSpec) -> Result<Output, ExecError> {
        lock(&self.calls).push(spec);
        lock(&self.responses)
            .pop_front()
            .unwrap_or_else(|| Ok(Output::default()))
    }
}

/// Locks a mutex, recovering from poisoning (the data is plain test bookkeeping).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A RealExec where `Program::Ip` is mapped to an arbitrary host tool.
    fn exec_with(tool: &str) -> RealExec {
        let path = ["/usr/bin", "/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("{tool} not found"));
        RealExec::new(ProgramPaths::from_map(HashMap::from([(Program::Ip, path)])))
    }

    #[tokio::test]
    async fn arguments_are_never_shell_interpreted() {
        let hostile = ["a;rm -rf /", "$(id)", "`id`", "a b", "*"];
        let out = exec_with("echo")
            .run(CommandSpec::new(Program::Ip, hostile))
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "a;rm -rf / $(id) `id` a b *\n"
        );
    }

    #[tokio::test]
    async fn environment_is_cleared() {
        let out = exec_with("env")
            .run(CommandSpec::new(Program::Ip, Vec::<OsString>::new()).env("EXTRA", "1"))
            .await
            .unwrap();
        let mut lines: Vec<_> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        assert_eq!(lines, ["EXTRA=1", "LC_ALL=C", &format!("PATH={SAFE_PATH}")]);
    }

    #[tokio::test]
    async fn stdin_is_delivered() {
        let out = exec_with("cat")
            .run(CommandSpec::new(Program::Ip, Vec::<OsString>::new()).stdin("secret"))
            .await
            .unwrap();
        assert_eq!(out.stdout, b"secret");
    }

    #[tokio::test]
    async fn failure_is_typed_with_stderr_tail() {
        let err = exec_with("ls")
            .run(CommandSpec::new(Program::Ip, ["/nonexistent-arilbox-path"]))
            .await
            .unwrap_err();
        match err {
            ExecError::Failed {
                program,
                code,
                stderr_tail,
            } => {
                assert_eq!(program, Program::Ip);
                assert_eq!(code, Some(2));
                assert!(stderr_tail.contains("nonexistent-arilbox-path"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_kills_the_child() {
        let started = std::time::Instant::now();
        let err = exec_with("sleep")
            .run(CommandSpec::new(Program::Ip, ["10"]).timeout(Duration::from_millis(100)))
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn output_is_bounded() {
        let out = exec_with("head")
            .run(CommandSpec::new(
                Program::Ip,
                ["-c", "3000000", "/dev/zero"],
            ))
            .await
            .unwrap();
        assert_eq!(out.stdout.len(), MAX_OUTPUT);
    }

    #[tokio::test]
    async fn unresolved_program_is_not_found() {
        let err = exec_with("echo")
            .run(CommandSpec::new(Program::Zfs, ["list"]))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ExecError::NotFound {
                program: Program::Zfs
            }
        ));
    }

    #[tokio::test]
    async fn service_runs_until_dropped() {
        let exec = exec_with("sleep");
        let mut child = exec
            .spawn_service(&CommandSpec::new(Program::Ip, ["30"]))
            .unwrap();
        assert!(child.try_wait().unwrap().is_none(), "still running");
        child.kill().await.unwrap();
    }

    #[test]
    fn resolve_fails_for_missing_program() {
        let err = ProgramPaths::resolve_in(&[Path::new("/nonexistent")], &[Program::Zfs]);
        assert!(matches!(
            err,
            Err(ExecError::NotFound {
                program: Program::Zfs
            })
        ));
    }

    #[tokio::test]
    async fn recording_exec_records_and_replays() {
        let exec = RecordingExec::new();
        exec.push_response(Err(ExecError::Failed {
            program: Program::Nft,
            code: Some(1),
            stderr_tail: String::new(),
        }));
        let spec = CommandSpec::new(Program::Nft, ["list", "ruleset"]);
        assert!(exec.run(spec.clone()).await.is_err());
        assert_eq!(exec.run(spec.clone()).await.unwrap(), Output::default());
        assert_eq!(exec.calls(), vec![spec.clone(), spec]);
    }
}
