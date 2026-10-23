# CLAUDE.md

Guidance for Claude (and humans) working in this repository.

> **Name:** **Arilbox** (an aril is the sealed seed pocket inside a pomegranate, like a container;
> box = container). The LXC backend crate is called **`arilxc`** (aril + LXC).
> Product name in UI and docs: `Arilbox`. In code, paths and binaries: `arilbox` (lowercase).

## What this project is

A self-hosted, Proxmox-like server management platform written in **Rust**.

- **Containers:** system containers via **LXC** (liblxc). This is the core of v1.
- **Virtual machines (later):** **Cloud Hypervisor** on KVM. **No QEMU anywhere.**
- **Host OS:** **Debian 13** for development and v1.
- **Not used:** QEMU, Incus/LXD, libvirt, Docker daemon on the host.

The full specification is in [`SPEC.md`](./SPEC.md). Read it before making architectural changes.
If code and spec disagree, raise it — do not silently diverge. Update `SPEC.md` in the same change
when a design decision changes.

## Repository layout

```
arilbox/
├── Cargo.toml                 # workspace
├── CLAUDE.md
├── SPEC.md
├── crates/
│   ├── arilbox-core/          # shared types, errors, validation (names, sizes), config
│   ├── arilxc/                # ContainerBackend trait + LxcCli impl (later: LxcFfi)
│   ├── arilbox-vm/            # VmBackend trait (stub in v1; Cloud Hypervisor later)
│   ├── arilbox-storage/       # ZFS / dir storage pools, datasets, snapshots
│   ├── arilbox-net/           # bridge + per-container network config
│   ├── arilbox-catalog/       # app manifests (parse, validate, install jobs)
│   ├── arilbox-api/           # axum routes, auth, sessions, job endpoints
│   └── arilbox-daemon/        # main binary `arilboxd` (systemd service)
├── web/                       # frontend (Svelte + Vite + TypeScript), built to static files
├── catalog/                   # built-in app manifests (TOML)
├── packaging/                 # systemd unit, Debian packaging, LXC templates, dev config
├── scripts/                   # dev-setup and helper scripts
└── tests/                     # integration tests (need root + LXC)
```

## Current status

- **Milestone:** M4 — Storage & snapshots (M1 — Skeleton, M2 — Containers and M3 — Auth + UI
  are done; see `SPEC.md` §19 for scope and acceptance criteria).
- Work milestone by milestone. Do not start features from a later milestone unless asked.
- When a milestone's acceptance criteria pass, update this line.

## Commands

```bash
# Build & check
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Unit tests (no root, no LXC needed — use the mock backend)
cargo test --workspace

# Integration tests (Debian 13 host with LXC installed, run as root). They create only
# arilbox-test-* containers, the bridge arilbox-test-0 and the nft table arilbox_test.
cargo build --workspace --all-targets --features lxc-integration
sudo -E env "PATH=$PATH" cargo test --workspace --features lxc-integration -- --test-threads=1

# Frontend
cd web && npm install && npm run dev      # dev server, proxies /api to arilboxd (ARILBOX_URL)
cd web && npm run check                   # svelte-check + i18n key check (must pass)
cd web && npm run build                   # outputs web/dist, served by arilboxd (paths.web_dir)

# Run the daemon locally: real LXC (root) or the in-memory mock (no root)
cargo build -p arilbox-daemon && sudo ./target/debug/arilboxd --config packaging/dev-config.toml
cargo run -p arilbox-daemon -- --config packaging/dev-config-mock.toml
```

## Development environment

Development and testing happen on a **Debian 13 (trixie) amd64** machine, normally a VM
(VMware, Hyper-V or Proxmox) reached from VS Code over Remote-SSH. Never develop or run
integration tests on a production server.

### Host requirements

- APT sources must include `main contrib non-free non-free-firmware`
  (`/etc/apt/sources.list.d/debian.sources`); ZFS lives in `contrib`, NVIDIA in `non-free`.
- Packages:
  ```bash
  sudo apt install -y build-essential pkg-config git curl ca-certificates linux-headers-amd64 \
    lxc lxc-templates uidmap dnsmasq-base nftables iproute2 apparmor \
    zfs-dkms zfsutils-linux nodejs npm
  ```
- Rust via **rustup** (not Debian's packaged Rust), with `clippy` and `rustfmt`. Edition 2024
  requires Rust 1.85+.
- Optional: `sqlx-cli` (`cargo install sqlx-cli --no-default-features --features sqlite`);
  `liblxc-dev clang libclang-dev` only for the future `LxcFfi` backend.
- Unprivileged containers: root must own subordinate IDs `100000-165535`
  (`usermod --add-subuids 100000-165535 --add-subgids 100000-165535 root`) and
  `/etc/lxc/default.conf` must contain the matching `lxc.idmap` lines.
- Sanity checks: `lxc-checkconfig` shows nothing missing; `stat -fc %T /sys/fs/cgroup`
  prints `cgroup2fs`.

### Storage for development

- The dev ZFS dataset is **`tank/arilbox`** (pool `tank` on a spare VM disk, or a file-backed
  pool for throwaway setups). `packaging/dev-config.toml` must point at it:
  ```toml
  [[storage]]
  name = "local-zfs"
  type = "zfs"
  dataset = "tank/arilbox"
  ```
- Production defaults (`rpool/arilbox`, `/etc/arilbox/...`) stay in `SPEC.md` §16; do not
  hard-code dev paths in code.

### GPU

- The dev VM normally has **no GPU**. GPU code paths must degrade cleanly when
  `capabilities.nvidia = false`, and unit tests must not require a GPU.
- GPU work (M5) is tested on a machine with an NVIDIA card, the Debian `nvidia-driver`
  package and `nvidia-container-toolkit` installed.

### KVM

- Not needed for v1. For future Cloud Hypervisor work, the VM needs nested virtualization
  (CPU type `host` / exposed virtualization extensions) so `/dev/kvm` exists.

## Test safety rules

- Integration tests create containers, datasets and snapshots **only** with the prefix
  `arilbox-test-` (e.g. `arilbox-test-create-01`) and only under `tank/arilbox/test/` (ZFS) or
  `/var/lib/arilbox/test/` (dir pool).
- Network tests use only the bridge `arilbox-test-0` (subnet `10.250.77.0/24`) and the nftables
  table `arilbox_test` (nft names can't contain `-`). The `arilxc` tests attach containers to
  LXC's own `lxcbr0` but never change it.
- Tests must clean up everything they create, including on failure (use a drop guard).
- Never stop, modify or delete a container, dataset or bridge that does not carry the test
  prefix — the dev machine may also hold the developer's own containers.
- Integration tests run serially (`--test-threads=1`).

## Tech stack

| Area | Choice |
|---|---|
| Language | Rust, edition 2024, stable toolchain |
| Async | `tokio` |
| HTTP API | `axum` + `tower-http` (static files, tracing, compression) |
| TLS | `rustls` (self-signed cert generated on first start) |
| Serialization | `serde`, `serde_json`, `toml` |
| Database | SQLite via `sqlx` (migrations in `crates/arilbox-api/migrations`) |
| Errors | `thiserror` in library crates, `anyhow` only in the daemon binary |
| Logging | `tracing` + `tracing-subscriber` (JSON in production) |
| Passwords | `argon2` (argon2id) |
| Frontend | Svelte + Vite + TypeScript, static build |

Do not add new dependencies casually. Prefer the standard library and the crates above.
Justify any new dependency in the PR/commit message.

## Architecture rules

1. **Backends sit behind traits.** All container operations go through `ContainerBackend`
   (`arilxc`), all VM operations through `VmBackend` (`arilbox-vm`). API code never shells out
   or calls liblxc directly.
2. **Long operations are jobs.** Create, delete, snapshot, restore, install and update return a
   job ID immediately; progress is reported through the jobs API. Never block an HTTP request
   on a multi-second operation.
3. **Data volumes live outside container root filesystems** (separate ZFS datasets / directories)
   so updates, reinstalls and backups are safe.
4. **The daemon is the only writer** to LXC config, storage and the database. The UI and any
   future CLI talk to the daemon over the REST API.
5. **Containers are unprivileged by default.** Privileged containers are not supported in v1.
6. **Containers must never require KVM.** KVM is only needed for the future VM feature, which
   must detect `/dev/kvm` at runtime and disable itself cleanly if absent.

## Security rules (non-negotiable)

- **Never pass user input through a shell.** Use `std::process::Command` / `tokio::process::Command`
  with separate arguments. No `sh -c`, no string-built commands.
- **Validate every identifier** with the shared validators in `arilbox-core`
  (container name regex: `^[a-z][a-z0-9-]{0,62}$`). Reject, don't sanitize.
- **No path traversal:** resolve and check all paths stay inside their configured root.
- **No `unwrap()` / `expect()` in non-test code** except for genuine invariants, with a comment.
- **`unsafe` is allowed only in a future `arilxc` FFI module**, and every `unsafe` block needs
  a `// SAFETY:` comment.
- Secrets (session keys, password hashes, API tokens) never appear in logs or API responses.
- App catalog manifests must pin images by digest (`@sha256:`); reject `:latest`.
- Exposed ports default to `internal`. Nothing is published to LAN or internet without an explicit
  user action.

## Coding conventions

- `cargo fmt` and `cargo clippy -D warnings` must pass.
- Public items in library crates get doc comments.
- Errors: one `Error` enum per crate (`thiserror`), mapped to HTTP status codes in `arilbox-api`.
- API JSON uses `snake_case` fields. Timestamps are RFC 3339 UTC.
- Keep functions small; prefer explicit types at API boundaries.
- Tests: unit tests next to code (`#[cfg(test)]`), mock backends for API tests, real LXC only in
  `--features lxc-integration` tests.

## UI text and languages

v1 ships **English only**. Multilingual support (Arabic, Urdu, RTL) is **optional / future**,
but keep the UI ready for it:

- All user-visible strings go through the i18n helper (`t('key')`) with English in
  `web/src/i18n/en.json`. No hard-coded strings in components.
- Use CSS logical properties (`margin-inline-start`, not `margin-left`) so RTL can be added later.
- App catalog manifests may include `name.ar`, `name.ur`, etc.; v1 displays `en` and ignores others.

## What not to do

- Do not introduce QEMU, libvirt, Incus or LXD.
- Do not run the Docker daemon on the host. Apps that need containers-in-containers run
  Podman inside an unprivileged LXC container.
- Do not make the API listen on plain HTTP outside development mode.
- Do not change the REST API shape without updating `SPEC.md` (section 7).
- Do not commit generated files (`web/dist`, `target/`, `web/node_modules/`), SQLite databases
  or local configs.
- Do not run `apt`, change system networking, or create/destroy ZFS pools as part of normal
  work. If the environment is missing something, say what and stop.

## Commit style

Conventional commits: `feat(lxc): add snapshot support`, `fix(api): ...`, `docs(spec): ...`.
Keep commits focused; one logical change each.