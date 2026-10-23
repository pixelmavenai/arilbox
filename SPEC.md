# Arilbox — Specification v1

Status: **Draft** · Product: **Arilbox** · LXC backend crate: `arilxc` · Target host: **Debian 13**

---

## 1. Overview

Arilbox is a self-hosted server management platform, similar in spirit to Proxmox VE, built in Rust.
It manages **LXC system containers** on a single Linux host through a web UI and a REST API, with
first-class **NVIDIA GPU sharing** and a **one-click app catalog**. Virtual machines via
**Cloud Hypervisor** (no QEMU) are planned for a later version and are designed for now.

### 1.1 Goals (v1)

- Manage unprivileged LXC containers: create, start, stop, restart, delete, console.
- Resource limits (CPU, memory, disk) per container.
- Snapshots and rollback.
- Storage on ZFS (preferred) or plain directories.
- Bridged networking with DHCP.
- Share an NVIDIA GPU with selected containers without installing drivers inside them.
- One-click app catalog (Ollama, Open WebUI, ComfyUI, Nextcloud, Cloudflare Tunnel).
- Secure single-admin web UI over HTTPS.

### 1.2 Non-goals (v1)

- Virtual machines (designed, not implemented — see §13).
- Multi-node clustering, HA, live migration.
- Multi-user roles and permissions (single admin only).
- Privileged containers.
- Multilingual UI — **optional / future** (see §15). v1 is English only, i18n-ready.
- Windows guests.

### 1.3 Hard constraints

- No QEMU, libvirt, Incus or LXD.
- Containers must work on hosts **without** KVM.
- Rust for all platform code. Frontend in TypeScript.

---

## 2. Glossary

| Term | Meaning |
|---|---|
| **Container** | An unprivileged LXC system container (full Linux userspace, shared host kernel). |
| **Template** | A base root filesystem used to create containers (from images.linuxcontainers.org or built locally). |
| **Volume** | A persistent data area (ZFS dataset or directory) mounted into a container, separate from its rootfs. |
| **Snapshot** | A point-in-time copy of a container's rootfs and, optionally, its volumes. |
| **App** | A catalog entry described by a manifest; installing an app creates a container configured for it. |
| **Job** | An asynchronous long-running operation with an ID, status and progress log. |
| **Storage pool** | A configured backing store: a ZFS dataset root or a directory root. |

---

## 3. System requirements

| Item | Requirement |
|---|---|
| OS | Debian 13 (amd64) |
| Kernel | Debian 13 stock kernel; cgroups v2 (unified hierarchy) |
| Packages | `lxc`, `lxc-templates`, `uidmap`, `zfsutils-linux` (optional), `dnsmasq-base` |
| CPU | Any x86_64. VT-x / AMD-V **not** required for v1 |
| RAM | 2 GB minimum for the platform; more for workloads |
| GPU (optional) | NVIDIA with host driver installed + `nvidia-container-toolkit` (provides `nvidia-container-cli`) |
| Privileges | `arilboxd` runs as root (required to manage LXC, storage and networking) |

---

## 4. Architecture

```
┌───────────────────────────────────────────────────────────┐
│ Web UI (Svelte, static files served by arilboxd)           │
├───────────────────────────────────────────────────────────┤
│ arilboxd — Rust daemon (axum + tokio)                      │
│   api · auth · jobs · catalog · storage · net · gpu       │
│   SQLite (state)  ·  /etc/arilbox/config.toml              │
├─────────────────────────────┬─────────────────────────────┤
│ ContainerBackend (v1)       │ VmBackend (future)          │
│   LxcCli → lxc-* tools      │   Cloud Hypervisor REST API │
│   (later: LxcFfi → liblxc)  │   over Unix socket          │
├─────────────────────────────┴─────────────────────────────┤
│ Kernel: namespaces · cgroups v2 · AppArmor · (KVM, VFIO)  │
│ ZFS / dirs · Linux bridge · NVIDIA LXC hook               │
├───────────────────────────────────────────────────────────┤
│ Debian 13                                                 │
└───────────────────────────────────────────────────────────┘
```

### 4.1 Crates

| Crate | Responsibility |
|---|---|
| `arilbox-core` | Shared types, IDs, validators, config loading, common errors |
| `arilxc` | `ContainerBackend` trait, `LxcCli` implementation, mock implementation for tests |
| `arilbox-vm` | `VmBackend` trait (stub, returns `Unsupported` in v1) |
| `arilbox-storage` | Storage pools, datasets/directories, quotas, snapshots |
| `arilbox-net` | Bridge management and per-container network config |
| `arilbox-catalog` | Manifest schema, validation, signature check, install/update/uninstall jobs |
| `arilbox-api` | axum router, auth, sessions, request validation, error mapping, SSE |
| `arilbox-daemon` | Binary `arilboxd`: config, startup checks, job runner, wiring |

---

## 5. Core components

### 5.1 ContainerBackend trait

```rust
#[async_trait::async_trait]
pub trait ContainerBackend: Send + Sync {
    async fn list(&self) -> Result<Vec<ContainerInfo>>;
    async fn info(&self, name: &ContainerName) -> Result<ContainerInfo>;
    async fn create(&self, spec: &CreateSpec) -> Result<()>;
    async fn start(&self, name: &ContainerName) -> Result<()>;
    async fn stop(&self, name: &ContainerName, timeout: Duration) -> Result<()>;
    async fn kill(&self, name: &ContainerName) -> Result<()>;
    async fn destroy(&self, name: &ContainerName) -> Result<()>;
    async fn set_limits(&self, name: &ContainerName, limits: &Limits) -> Result<()>;
    async fn snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    async fn restore(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    async fn delete_snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    async fn list_snapshots(&self, name: &ContainerName) -> Result<Vec<SnapshotInfo>>;
    async fn exec(&self, name: &ContainerName, cmd: &[String]) -> Result<ExecOutput>;
}
```

- **v1 implementation:** `LxcCli`, calling `lxc-create`, `lxc-start`, `lxc-stop`, `lxc-info`,
  `lxc-ls`, `lxc-snapshot`, `lxc-attach`, `lxc-destroy` with argument vectors (never a shell).
- **Later:** `LxcFfi` via `bindgen` against `liblxc` (`lxccontainer.h`), same trait.
- **Tests:** `MockBackend` (in-memory) for API and job tests.

`ContainerInfo` fields: `name`, `state` (`stopped|starting|running|stopping|frozen|error`),
`ipv4[]`, `ipv6[]`, `pid`, `cpu_usage_ns`, `memory_bytes`, `created_at`, `template`, `gpu` (bool),
`app_id` (optional).

### 5.2 Container naming

- Regex: `^[a-z][a-z0-9-]{0,62}$`, must not end in `-`.
- Unique per host. Reserved names: `arilbox`, `host`, `default`.
- Snapshot names: `^[a-z0-9][a-z0-9._-]{0,62}$`.

### 5.3 Container configuration

Each container gets an LXC config at `/var/lib/lxc/<name>/config` generated by the daemon from
`CreateSpec` + `Limits`. The daemon owns this file; manual edits may be overwritten. It is
rendered deterministically (golden tests in `crates/arilxc/tests/fixtures/`) and written
atomically, mode `0640`.

Complete config (values in `<>` vary):

```
# Managed by arilboxd — manual edits will be overwritten
lxc.uts.name = <name>
lxc.arch = <amd64|arm64>
lxc.rootfs.path = <dir:/path | zfs:dataset>
lxc.idmap = u 0 100000 65536
lxc.idmap = g 0 100000 65536
lxc.apparmor.profile = generated
lxc.apparmor.allow_nesting = 0
lxc.seccomp.profile = /usr/share/lxc/config/common.seccomp
lxc.mount.auto = cgroup:mixed proc:mixed sys:mixed
lxc.mount.entry = /sys/fs/fuse/connections sys/fs/fuse/connections none bind,optional 0 0
lxc.pty.max = 1024
lxc.tty.max = 4
lxc.cgroup2.memory.max = <bytes | max>
lxc.cgroup2.memory.swap.max = <bytes | max>
lxc.cgroup2.cpu.max = <cores × 100000 | max> 100000
lxc.cgroup2.pids.max = <n, default 4096>
lxc.start.auto = <0|1>
lxc.net.0.type = veth
lxc.net.0.link = <bridge>
lxc.net.0.flags = up
lxc.net.0.name = eth0
lxc.net.0.hwaddr = 00:16:3e:<random, fixed at creation>
```

- **No `lxc.include`.** The security baseline that LXC's distro includes (`common.conf` +
  `userns.conf`) give an unprivileged container is written out as fixed lines (seccomp profile,
  `mount.auto`, pty/tty limits, fuse mount). Device and capability rules are omitted, as in
  upstream `userns.conf`: the user namespace already denies them.
- `nesting = true` (needed for Podman inside a container) sets `lxc.apparmor.allow_nesting = 1`
  and adds `lxc.mount.entry = proc dev/.lxc/proc proc create=dir,optional 0 0` and
  `lxc.mount.entry = sys dev/.lxc/sys sysfs create=dir,optional 0 0`.
- The MAC is generated once so DHCP leases survive restarts.
- **Managed containers only.** Next to the config, the daemon writes `arilbox.json` (template,
  storage pool, rootfs, MAC, bridge, limits, flags, `created_at`). Containers without it are
  invisible to the API and are never started, stopped or deleted by Arilbox.

### 5.4 Resource limits

| Limit | Mechanism | v1 |
|---|---|---|
| CPU | `cpu.max` (quota/period) — expressed to users as number of cores | ✅ |
| Memory | `memory.max` | ✅ |
| Swap | `memory.swap.max` | ✅ |
| Disk | ZFS `refquota` on the container dataset; not enforced on `dir` pools | ✅ (ZFS) |
| Process count | `pids.max` | ✅ (default 4096) |
| Network bandwidth | — | ❌ later |

Limits can be changed on running containers where the kernel allows it.

### 5.5 Templates

- Source: `lxc-create -t download` (images.linuxcontainers.org) for `debian/13`, `ubuntu/24.04`,
  `almalinux/10`, `alpine/edge`.
- Only these four are accepted (`400 invalid_template` otherwise). The image server names some
  releases by codename: `debian/13` → `trixie`, `ubuntu/24.04` → `noble`.
- `lxc-create` is given the generated config as its seed so the rootfs is unpacked with the
  idmap; the daemon then replaces the config written by the template.
- Downloaded images are cached by LXC.
- Platform-built template `app-runner-debian13` (Debian 13 + Podman + nesting enabled) is used by the
  app catalog. Built with `distrobuilder` from `packaging/templates/app-runner.yaml`.

---

## 6. Storage

### 6.1 Pools

Configured in `/etc/arilbox/config.toml`:

```toml
[[storage]]
name = "local-zfs"
type = "zfs"
dataset = "rpool/arilbox"       # root dataset, created if missing

[[storage]]
name = "local-dir"
type = "dir"
path = "/var/lib/arilbox/storage"
```

### 6.2 Layout (ZFS)

```
rpool/arilbox/containers/<name>          # container rootfs (lxc.rootfs.path = zfs backing)
rpool/arilbox/volumes/<name>/<volume>    # data volumes, mounted into containers
```

- Snapshots use `zfs snapshot` on the rootfs dataset; volume snapshots are optional per request.
- `dir` pools use LXC `dir` backing; snapshots fall back to copying (slow, documented).

### 6.3 Volumes

- Created with a size (ZFS `refquota`) and a mount path inside the container.
- Mounted via `lxc.mount.entry` with correct idmapped ownership.
- Survive container deletion if `keep_volumes = true` is passed to delete.

---

## 7. REST API (v1)

Base path: `/api/v1`. JSON in and out. Auth via session cookie (§12): every endpoint needs a
valid session (`401 unauthenticated`) except `/health`, `/auth/setup` and `/auth/login`. Errors
use:

```json
{ "error": { "code": "container_not_found", "message": "Container 'web' does not exist" } }
```

| Status | Used for |
|---|---|
| 200 / 201 | Success (synchronous) |
| 202 | Job accepted — body `{ "job_id": "..." }` |
| 400 | Validation error |
| 204 | Success without a body (logout, password change) |
| 401 / 403 | Not logged in / CSRF failure |
| 404 | Not found |
| 409 | Conflict (name exists, wrong state) |
| 429 | Too many failed login attempts (`Retry-After` header in seconds) |
| 500 | Internal error (logged with request ID) |

Every response carries an `X-Request-Id` header (the client's value if it sent one, otherwise a
generated UUID). The same ID appears in the daemon's log lines for that request.

### 7.0 Health

| Method | Path | Description |
|---|---|---|
| GET | `/health` | Liveness check, `{ "status": "ok" }`. Never requires a session. |

### 7.1 Auth

| Method | Path | Description |
|---|---|---|
| GET | `/auth/setup` | Public. `{ "required": bool }`: true until the admin user exists |
| POST | `/auth/setup` | Public. `{ "token", "username", "password" }` → 201 `{ "username" }` + session cookie |
| POST | `/auth/login` | Public. `{ "username", "password" }` → 200 `{ "username" }` + session cookie |
| POST | `/auth/logout` | Ends the session → 204, cookie cleared |
| GET | `/auth/me` | Current user, `{ "username" }` |
| POST | `/auth/password` | Change password `{ "current", "new" }` → 204; the user's other sessions end |

Error codes: `invalid_username` (400), `weak_password` (400, fewer than 10 characters),
`wrong_password` (400, `current` is wrong), `invalid_credentials` (401),
`invalid_setup_token` (403), `setup_required` (409, login before setup), `setup_done` (409),
`too_many_attempts` (429). Login, setup and password change share the per-IP failure limit.
Usernames match `^[a-z_][a-z0-9_-]{0,31}$`; passwords are 10 characters to 1024 bytes.

### 7.2 Host

| Method | Path | Description |
|---|---|---|
| GET | `/host` | Hostname, OS, kernel, uptime, CPU/RAM totals, platform version (see below) |
| GET | `/host/stats` | Current CPU %, memory, swap, load (see below; per-pool usage arrives with M4) |
| GET | `/host/gpus` | Detected GPUs (via `nvidia-smi --query-gpu ... --format=csv`), memory, utilization |
| GET | `/host/capabilities` | `{ "zfs": bool, "nvidia": bool, "kvm": bool, "cgroup_v2": bool }` |

`GET /host` response:

```json
{
  "hostname": "pve1",
  "os": "Debian GNU/Linux 13 (trixie)",
  "kernel": "6.12.48+deb13-amd64",
  "uptime_s": 86400,
  "cpu_count": 16,
  "memory_total_bytes": 68719476736,
  "platform_version": "0.1.0"
}
```

`GET /host/stats` response (`cpu_percent` is measured since the previous call):

```json
{
  "cpu_percent": 12.5,
  "memory_total_bytes": 68719476736, "memory_used_bytes": 8589934592,
  "swap_total_bytes": 0, "swap_used_bytes": 0,
  "load": [0.5, 0.25, 0.1],
  "uptime_s": 86400
}
```

Capability detection: `zfs` = `/dev/zfs` exists; `nvidia` = `nvidia-smi` installed and
`/dev/nvidia0` exists; `kvm` = `/dev/kvm` exists; `cgroup_v2` =
`/sys/fs/cgroup/cgroup.controllers` exists.

### 7.3 Containers

| Method | Path | Description |
|---|---|---|
| GET | `/containers` | List `ContainerInfo` |
| POST | `/containers` | Create → 202 job |
| GET | `/containers/{name}` | `ContainerInfo` (config and limits are added with limit editing in M4) |
| PATCH | `/containers/{name}` | Update limits / autostart / description |
| DELETE | `/containers/{name}?keep_volumes=bool&force=bool` | Delete → 202 job (must be stopped, or `force=true` to kill first). `keep_volumes` takes effect with volumes (M4) |
| POST | `/containers/{name}/start` | → 202 job (`409 container_running` unless stopped) |
| POST | `/containers/{name}/stop` | Body optional, `{ "timeout_s": 30 }` (1–600) → 202 job (`409 container_stopped` unless running) |
| POST | `/containers/{name}/restart` | Body as stop → 202 job; a stopped container is just started |
| GET | `/containers/{name}/console?rows=24&cols=80` | WebSocket: interactive root shell (`lxc-attach` on a PTY), see below |
| GET | `/containers/{name}/stats` | CPU, memory, network counters |

Create request:

```json
{
  "name": "ai",
  "template": { "distro": "debian", "release": "13" },
  "storage": "local-zfs",
  "limits": { "cores": 4, "memory_mb": 8192, "swap_mb": 0, "disk_gb": 50 },
  "network": { "bridge": "arilbox0", "mode": "dhcp" },
  "gpu": true,
  "nesting": true,
  "autostart": true,
  "start": true,
  "volumes": [ { "name": "data", "size_gb": 100, "mount": "/srv/data" } ]
}
```

- Only `name` is required. `template` defaults to `defaults.template`, `storage` to
  `defaults.storage` (else the first pool), `network.bridge` to the managed bridge, `limits` to
  unlimited (`pids_max` 4096), booleans to `false`.
- `start: true` starts the container and waits up to 60 s for an IPv4 address as part of the
  create job; if that fails, the container is removed again.
- `limits` fields: `cores` 1–1024, `memory_mb` 16–16777216, `swap_mb` ≤ 16777216, `disk_gb`
  1–1048576, `pids_max` 16–4194304.
- Until their milestones: `gpu: true` → `409 gpu_unavailable` without an NVIDIA GPU, else
  `400 unsupported` (M5); non-empty `volumes` → `400 unsupported` (M4).

Error codes used by the container endpoints: `invalid_name`, `invalid_request` (malformed JSON,
unknown field, bad query), `invalid_template`, `invalid_storage`, `invalid_limits`,
`invalid_bridge`, `unsupported`, `csrf_failed` (403), `container_not_found` (404),
`container_exists`, `container_running`, `container_stopped`, `container_wrong_state`,
`storage_unavailable`, `gpu_unavailable`, `target_busy` (409).

Console protocol: binary frames carry raw terminal bytes in both directions; text frames from
the client are JSON control messages, `{ "type": "resize", "rows": 40, "cols": 120 }`
(1–1000 each). The server closes the socket when the shell exits, and closing the socket ends
the shell. Because browsers cannot add the CSRF header to a WebSocket handshake, the `Origin`
header must match the request's host (`403 origin_mismatch`). The container must be running
(`409 container_stopped`).

Choices for the create form:

| Method | Path | Description |
|---|---|---|
| GET | `/storage` | Configured pools: `[{ "name", "type": "zfs"\|"dir", "available", "default" }]` |
| GET | `/templates` | Allowed templates (§5.5): `[{ "distro", "release", "default" }]` |

### 7.4 Snapshots

| Method | Path | Description |
|---|---|---|
| GET | `/containers/{name}/snapshots` | List |
| POST | `/containers/{name}/snapshots` | `{ "name", "include_volumes": bool }` → 202 job |
| POST | `/containers/{name}/snapshots/{snap}/restore` | → 202 job (container stopped first) |
| DELETE | `/containers/{name}/snapshots/{snap}` | → 202 job |

### 7.5 Jobs

| Method | Path | Description |
|---|---|---|
| GET | `/jobs?limit=50` | Recent jobs, newest first (`limit` 1–500) |
| GET | `/jobs/{id}` | Status: `queued|running|succeeded|failed|cancelled`, steps, error (`404 job_not_found`) |
| GET | `/jobs/{id}/events` | Server-Sent Events stream of step/progress/log lines |

Job object:

```json
{
  "id": "0192f0c4-…", "kind": "container_create", "target": "ai", "status": "failed",
  "steps": [
    { "id": "create_container", "label": "Creating container", "status": "compensated",
      "started_at": "…", "finished_at": "…", "log": [ { "at": "…", "message": "…" } ] }
  ],
  "error": { "code": "network_timeout", "message": "…", "step": "wait_for_network",
             "rollback_incomplete": false },
  "created_at": "…", "started_at": "…", "finished_at": "…"
}
```

Step status: `pending|running|succeeded|failed|compensated|compensation_failed`.

SSE events (event name = `type` in the JSON data): first `snapshot` (the whole job object), then
`step_started { step }`, `log { step, at, message }`, `step_finished { step, status }`, and finally
`job_finished { status, error }`, after which the stream closes. A finished job sends only the
snapshot. A client that falls behind receives a fresh `snapshot`.

### 7.6 Catalog & apps

| Method | Path | Description |
|---|---|---|
| GET | `/catalog` | Available apps (manifest summaries) |
| GET | `/catalog/{app_id}` | Full manifest + settings form schema |
| POST | `/apps` | Install `{ "app_id", "name", "settings": {...} }` → 202 job |
| GET | `/apps` | Installed apps (container + app version + health) |
| POST | `/apps/{name}/update` | → 202 job (snapshot → update → health check → auto-rollback) |
| DELETE | `/apps/{name}?keep_data=bool` | Uninstall → 202 job |

---

## 8. Jobs

- Stored in SQLite: `id` (UUIDv7), `kind`, `target`, `status`, `steps` (JSON), `error`,
  `created_at`, `started_at`, `finished_at`.
- A single in-process job runner (tokio) executes jobs; **one active job per target** (container
  name) to prevent conflicting operations; different targets run concurrently.
- Each job is a list of named steps; each step records start/end and log lines.
- On daemon restart, `running` jobs are marked `failed` with reason `daemon_restarted`.
- Failed multi-step jobs run their compensation steps (e.g. delete half-created container).
- A request for a target that already has an active job gets `409 target_busy`.

Container job kinds and step ids (stable):

| Kind | Steps |
|---|---|
| `container_create` | `create_container` (+ `start_container`, `wait_for_network` when `start`) |
| `container_start` | `start_container`, `wait_for_network` |
| `container_stop` | `stop_container` |
| `container_restart` | `stop_container`, `start_container`, `wait_for_network` |
| `container_delete` | (`kill_container` when forced), `destroy_container` |

A failing `wait_for_network` in `container_start` / `container_restart` leaves the container
running (the user asked for it to run); in `container_create` it rolls the whole create back.

---

## 9. Networking

- v1 uses one managed Linux bridge, default `arilbox0`, created by the daemon via `ip link`
  (rtnetlink crate allowed) with a private subnet (default `10.88.0.1/24`). Setup runs at every
  start and is idempotent; an existing interface with that name that is not a bridge stops the
  daemon. The daemon enables `net.ipv4.ip_forward`.
- DHCP + DNS for containers via `dnsmasq` bound to the bridge, supervised by the daemon
  (restarted with backoff if it exits): `--keep-in-foreground --conf-file= --bind-interfaces
  --interface=<bridge> --listen-address=<bridge ip>`, DHCP range from the address after the
  bridge's to the last host address, 1 h leases, lease file in `<data_dir>/net/`.
- NAT for outbound traffic via nftables masquerade rule in a dedicated `arilbox` table, loaded
  atomically with `nft -f -`:
  `ip saddr <subnet> ip daddr != <subnet> masquerade` in a `postrouting` nat chain. No other
  rules; host firewall policies are left alone.
- Containers get a `veth` pair attached to the bridge.
- Optional: attach to an existing host bridge (e.g. `vmbr0`) for LAN IPs.
- **No inbound port forwarding in v1** except via the Cloudflare Tunnel app or explicit LAN bridge.

---

## 10. GPU sharing (NVIDIA)

- Detection: `nvidia-smi` present and `/dev/nvidia0` exists → `capabilities.nvidia = true`.
- Enabling GPU on a container adds:

```
lxc.hook.mount = /usr/share/lxc/hooks/nvidia
lxc.environment = NVIDIA_VISIBLE_DEVICES=all
lxc.environment = NVIDIA_DRIVER_CAPABILITIES=compute,utility
```

- The hook uses `nvidia-container-cli` to inject the host driver libraries and device nodes —
  **no driver installation inside containers**.
- Multiple containers may share the GPU simultaneously.
- For Podman inside a GPU container, the `app-runner` template configures CDI
  (`nvidia-ctk cdi generate`) at first boot.
- Selecting specific GPUs (`NVIDIA_VISIBLE_DEVICES=0,1`) is supported in the API; UI shows "all" in v1.

---

## 11. App catalog

### 11.1 Manifest (TOML)

Located at `catalog/apps/<app_id>/app.toml` plus `icon.svg`.

```toml
id = "ollama"
version = "1.0.0"
category = "ai"

[name]
en = "Ollama"          # other languages optional, ignored in v1

[description]
en = "Run large language models locally"

[runtime]
type = "oci"                               # oci | script (v1); image (later)
base = "app-runner-debian13"
image = "docker.io/ollama/ollama@sha256:<digest>"

[resources]
cores = 4
memory_mb = 8192
disk_gb = 20
gpu = "required"                           # required | optional | none

[[volumes]]
name = "models"
mount = "/root/.ollama"
size_gb = 100
backup = true

[[ports]]
name = "api"
internal = 11434
expose = "internal"                        # internal | lan | tunnel

[[settings]]
key = "OLLAMA_KEEP_ALIVE"
label = { en = "Keep models loaded for" }
type = "select"                            # string | number | bool | select | secret
options = ["5m", "1h", "forever"]
default = "5m"

[health]
http = "http://127.0.0.1:11434/api/tags"
timeout_s = 60
```

Multi-service apps (e.g. Nextcloud + Postgres + Redis) use `[[services]]` blocks, run as one
Podman pod inside one container.

### 11.2 Validation rules

- `image` must be pinned by digest; `:latest` or tag-only images are rejected.
- `id` follows the container name regex.
- `gpu = "required"` apps cannot be installed if `capabilities.nvidia = false`.
- `secret` settings are stored encrypted at rest and never returned by the API.

### 11.3 Install job steps

1. Validate manifest, settings and capacity (RAM, disk, GPU, name conflict).
2. Create container from `base` template with limits, nesting, GPU if needed.
3. Create volumes.
4. Start container; wait for network.
5. Write a Podman Quadlet unit (`/etc/containers/systemd/<app>.container`) from the manifest.
6. Pull image; `systemctl daemon-reload && systemctl start <app>`.
7. Health check until `timeout_s`.
8. Record installed app; report URL.

Failure at any step → compensation (stop + destroy container, keep nothing unless volumes pre-existed).

### 11.4 Update job

Snapshot container (+ volumes) → update image digest → restart → health check →
on failure **restore snapshot automatically** and mark job failed with reason.

### 11.5 Catalog source

- v1: built-in catalog shipped in `catalog/` and installed to `/usr/share/arilbox/catalog`.
- Later: remote Git catalogs with signature verification (minisign); third-party catalogs flagged.

### 11.6 v1 apps

`ollama`, `open-webui`, `comfyui`, `nextcloud` (stack), `cloudflared`.

---

## 12. Authentication & security

- **Single admin user** in v1, created at first start: while no user exists, the daemon prints a
  one-time setup URL with a random 256-bit token to the journal (`/setup?token=…`); the first
  visit sets username + password. The token is single-use, replaced on every restart and only
  its hash is kept in memory. This is the only secret ever logged.
- Passwords hashed with **argon2id** (memory 64 MiB, iterations 3, parallelism 1), at most two
  hashes at a time. Logins for unknown users verify against a dummy hash so timing does not
  reveal which usernames exist.
- Sessions: random 256-bit ID, server-side in SQLite (only its SHA-256 is stored), cookie
  `arilbox_session` with `Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=604800`
  (`Secure` is dropped only for plain-HTTP dev mode), idle timeout 8 h, absolute timeout 7 days.
  Changing the password ends the user's other sessions.
- CSRF: `SameSite=Strict` + required header `X-Arilbox-Request: 1` on all mutating requests;
  the console WebSocket checks `Origin` instead (§7.3).
- Login rate limiting: 5 failures / 15 min per client IP (login, setup and password change),
  then `429 too_many_attempts` until the oldest failure leaves the window. Kept in memory. Behind
  a reverse proxy all clients share the proxy's IP.
- Security headers on every response: a strict `Content-Security-Policy` (same origin only, no
  inline scripts, `frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`,
  `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer`. API responses are `no-store`.
- HTTPS only (`rustls`), default bind `0.0.0.0:8443`; self-signed cert generated at first start
  in `/etc/arilbox/tls/`; user can replace cert and key.
- Containers unprivileged with generated AppArmor profiles; no privileged mode in v1.
- Audit log table: login, logout, failed login, every mutating API call (user, action, target, time).
  Failed logins record the attempted username as the target and no user.
- Optional TOTP 2FA: **later** (M6).

---

## 13. Virtual machines (designed, not in v1)

- `VmBackend` trait mirrors `ContainerBackend` (list, create, start, stop, destroy, snapshot,
  console, set_limits).
- Implementation: one `cloud-hypervisor` process per VM with `--api-socket`; daemon talks HTTP over
  the Unix socket.
- Firmware: `rust-hypervisor-firmware` (Linux cloud images) and Cloud Hypervisor EDK2 build
  (UEFI, Windows).
- Disks: raw/qcow2 on ZFS zvols or files; networking via TAP devices on `arilbox0`.
- GPU: VFIO passthrough (whole device per VM).
- Feature flag: enabled only when `capabilities.kvm = true`; UI hides VM features otherwise.
- **No QEMU** under any circumstances.

---

## 14. Web UI (v1)

| Page | Contents |
|---|---|
| Login / first-run setup | Username, password; setup token flow |
| Dashboard | Host CPU/RAM/disk, GPU status, running containers, recent jobs |
| Containers | List with state, IP, CPU/RAM; start/stop/restart; create wizard |
| Container detail | Overview, limits (editable), snapshots, volumes, console (xterm.js), logs |
| App catalog | Grid of apps; install dialog generated from manifest settings |
| Installed apps | Health, URL, update, uninstall (keep data?) |
| Jobs | List + live progress (SSE) |
| Settings | Storage pools, network, TLS cert, password |

- Responsive down to phone width.
- Light/dark theme (follows the system unless chosen in Settings).
- All strings through `t('key')` (see §15). Server error `code`s are translated as
  `error.<code>`, job steps as `step.<id>`.
- Single-page app with History-API routes (`/`, `/setup`, `/login`, `/containers`,
  `/containers/{name}`, `/jobs`, `/jobs/{id}`, `/settings`); the daemon answers every non-`/api`
  path it has no file for with `index.html`. Built assets live under `/assets/` with hashed names.
- M3 ships login/setup, dashboard (without disk and GPU details), containers, container detail
  (overview, console, jobs), jobs and settings (password, theme). Limits editing, snapshots and
  volumes arrive with M4; catalog pages with M5.

---

## 15. Internationalization (optional / future)

- **v1: English only.**
- The UI must be **i18n-ready** so Arabic, Urdu or other languages can be added without refactoring:
  - every user-facing string uses `t('key')` with `web/src/i18n/en.json`;
  - layout uses CSS logical properties (`inline-start/end`) to allow RTL later;
  - dates and numbers formatted via `Intl` APIs;
  - manifests may carry extra languages; v1 reads `en` only.
- Adding a language later = add `<lang>.json`, a language picker, and `dir="rtl"` handling.

---

## 16. Configuration

`/etc/arilbox/config.toml`:

```toml
[server]
bind = "0.0.0.0:8443"
tls_cert = "/etc/arilbox/tls/cert.pem"
tls_key = "/etc/arilbox/tls/key.pem"
tls = true                             # false only allowed with dev_mode = true
dev_mode = false                       # human-readable logs; permits plain HTTP

[paths]
data_dir = "/var/lib/arilbox"          # SQLite DB, job logs
catalog_dir = "/usr/share/arilbox/catalog"
web_dir = "/usr/share/arilbox/web"     # built UI (web/dist); API only if index.html is missing

[containers]
backend = "lxc"                        # "mock" (in-memory) only allowed with dev_mode = true
lxc_path = "/var/lib/lxc"

[network]
bridge = "arilbox0"
subnet = "10.88.0.1/24"

[[storage]]
name = "local-zfs"
type = "zfs"
dataset = "rpool/arilbox"

[defaults]
storage = "local-zfs"
template = { distro = "debian", release = "13" }
```

- Every key is optional; the values above are the defaults (`defaults.storage` has no default and,
  when set, must name a configured pool).
- Unknown keys are rejected so typos fail at start-up.
- The self-signed certificate is generated only when **both** `tls_cert` and `tls_key` are missing;
  if just one exists, start-up fails rather than overwriting it.

---

## 17. Observability

- `tracing` logs to journald (JSON fields); each API request has a `request_id`.
- `/api/v1/host/stats` and `/containers/{name}/stats` read cgroup v2 files directly
  (`cpu.stat`, `memory.current`, `pids.current`).
- Metrics history (graphs) stored as 1-minute samples for 24 h in SQLite (later: longer retention).

---

## 18. Testing strategy

| Level | Scope | Runs where |
|---|---|---|
| Unit | Validators, config, manifest parsing, LXC config generation, error mapping | CI, any machine |
| API | axum handlers against `MockBackend` + in-memory SQLite | CI |
| Integration | Real LXC: create/start/stop/snapshot/destroy, GPU hook (if present) | Debian 13 VM/host, root, `--features lxc-integration` |
| End-to-end | UI flows (Playwright) against a dev daemon with `MockBackend` | CI |

---

## 19. Milestones

| Milestone | Scope | Acceptance criteria |
|---|---|---|
| **M1 — Skeleton** | Workspace, config, `arilboxd` boots, HTTPS, health endpoint, `MockBackend` | `cargo test` green; `GET /api/v1/host` returns host info |
| **M2 — Containers** | `LxcCli`: list/create/start/stop/delete; jobs + SSE; bridge + DHCP | Create a Debian 13 container via API, it gets an IP and reaches the internet |
| **M3 — Auth + UI** | First-run setup, login, sessions, CSRF; dashboard, containers list/detail, console | Full container lifecycle from the browser |
| **M4 — Storage & snapshots** | ZFS pools, volumes, quotas, snapshots/restore, limits editing | Snapshot → modify → restore returns container to prior state |
| **M5 — GPU + catalog** | NVIDIA hook, `app-runner` template, catalog with 5 apps, update with auto-rollback | Install Ollama with GPU in one click; `nvidia-smi` works inside; failed update rolls back |
| **v1.0** | M1–M5 complete, Debian package, docs | Fresh Debian 13 → install `.deb` → working platform |
| M6 (v1.x) | TOTP 2FA, API tokens, scheduled backups (restic), metrics graphs | — |
| M7 (v2) | Cloud Hypervisor VMs, `LxcFfi`, custom installer ISO, optional multilingual UI | — |

---

## 20. Open questions

1. Name availability check for **Arilbox** (domain, GitHub org, crates.io, trademark).
2. Frontend framework confirmation (default: Svelte + Vite + TypeScript).
3. When to move from `LxcCli` to `LxcFfi` (performance or control needs).
4. Backup target formats for M6 (restic repository vs ZFS send streams).
5. Licensing of the platform itself (e.g. Apache-2.0 vs AGPL-3.0) — liblxc is LGPL-2.1+,
   Cloud Hypervisor is Apache-2.0/BSD, both compatible with either choice.