---
name: run-command-safely
description: Use whenever Arilbox code needs to run an external program (lxc-*, zfs, zpool, ip, nft, nvidia-smi, dnsmasq). Defines the only allowed way to spawn processes — no shell, validated arguments, timeouts and typed errors.
---

# Running external commands safely

Arilbox runs as root and passes user-supplied names to system tools. A single shell injection
or unvalidated argument is a host compromise. Every process spawn in the codebase goes through
one helper; nothing else may call `Command::new` directly.

## The rules

1. **No shell, ever.** Never `sh -c`, `bash -c`, or a string that is later split. Build an
   argument vector with one element per argument.
2. **Allow-listed programs only.** Programs are an enum, resolved to absolute paths once at
   daemon start (fail start-up if a required one is missing):
   `LxcCreate, LxcStart, LxcStop, LxcInfo, LxcLs, LxcSnapshot, LxcAttach, LxcDestroy,
   LxcCgroup, Zfs, Zpool, Ip, Nft, NvidiaSmi, Dnsmasq`. Adding a program means adding a variant
   and saying why in the commit message.
3. **Only validated values become arguments.** User input reaches a command only as a validated
   newtype from `arilbox-core` (`ContainerName`, `SnapshotName`, `DatasetPath`, `VolumeName`,
   `BridgeName`, …). Raw `String`/`&str` from a request never goes into `args`.
4. **End options before positional values** with `--` where the tool supports it, so a value can
   never be parsed as a flag. Validators also reject values starting with `-`.
5. **Clean environment.** `env_clear()`, then set only `PATH=/usr/sbin:/usr/bin:/sbin:/bin` and
   `LC_ALL=C` (stable, parseable output). Add other variables explicitly per call if needed.
6. **Always a timeout.** Default 30 s; long operations (image download, `zfs destroy -r`) pass an
   explicit longer timeout. Use `kill_on_drop(true)`; on timeout kill the child and return
   `ExecError::Timeout`.
7. **Bounded output.** Capture stdout/stderr, truncate each to 1 MiB. Never stream unbounded
   output into memory.
8. **Typed errors.** Non-zero exit → `ExecError::Failed { program, code, stderr_tail }` where
   `stderr_tail` is at most the last 2 KiB. Callers map this to their own crate error; the API
   never returns raw stderr to the client, only a stable error `code` (details go to logs).
9. **Logging.** Log at `debug`: program and the validated identifiers. Never log secrets
   (setting values marked `secret`, tokens, passwords). If an argument could hold a secret, pass
   it through a file descriptor or stdin instead of argv (argv is visible in `ps`).

## Shape of the helper (lives in `arilbox-core::exec`)

```rust
pub enum Program { LxcCreate, LxcStart, /* … */ Zfs, Ip, Nft }

pub struct CommandSpec {
    pub program: Program,
    pub args: Vec<OsString>,
    pub timeout: Duration,
    pub stdin: Option<Vec<u8>>,
}

pub struct Output { pub stdout: Vec<u8>, pub stderr: Vec<u8> }

#[async_trait::async_trait]
pub trait Exec: Send + Sync {
    async fn run(&self, spec: CommandSpec) -> Result<Output, ExecError>;
}
```

- `RealExec` spawns with `tokio::process::Command` following the rules above.
- `RecordingExec` (tests) records each `CommandSpec` and returns scripted outputs.

## Write builders as pure functions

Split "what to run" from "running it", so argument building is unit-tested without root:

```rust
pub fn snapshot_cmd(name: &ContainerName, snap: &SnapshotName) -> CommandSpec {
    CommandSpec {
        program: Program::LxcSnapshot,
        args: vec!["-n".into(), name.as_os_str().into(), "-N".into(), snap.as_os_str().into()],
        timeout: Duration::from_secs(60),
        stdin: None,
    }
}
```

## Checklist before finishing

- [ ] No `Command::new` outside `arilbox-core::exec` (`grep -rn "Command::new" crates/`).
- [ ] Every argument from user input is a validated newtype.
- [ ] A timeout is set and appropriate for the operation.
- [ ] Builder function has unit tests asserting the exact argument vector.
- [ ] A test shows a hostile value (`"-rf"`, `"a;rm -rf /"`, `"../x"`, `"a\nb"`) is rejected by
      validation before any command is built.
- [ ] Error path maps to a stable error code; stderr is not exposed to API clients.
