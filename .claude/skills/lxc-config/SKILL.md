---
name: lxc-config
description: Use when generating or changing LXC container configuration in Arilbox (idmap, cgroup v2 limits, AppArmor, nesting, mounts, networking, NVIDIA GPU hook). Keeps container configs secure, deterministic and tested.
---

# Generating LXC configuration

The LXC config file is the security boundary between a container and the host. Most container
escapes come from a bad config line. Arilbox generates every config from typed data; it never
copies user text into the file.

Source of truth: `SPEC.md` §5.3 (defaults), §5.4 (limits), §6.3 (volumes), §9 (network),
§10 (GPU). If the spec does not say what a line should be, **stop and ask** — do not guess or
copy lines from a blog post.

## How configs are produced

1. Build a typed `ContainerConfig` (in `arilxc`) from `CreateSpec` + `Limits` + volumes + GPU flag.
2. Render it with a **pure function** `render_config(&ContainerConfig) -> String`:
   - deterministic key order (same input → byte-identical output);
   - first line: `# Managed by arilboxd — manual edits will be overwritten`;
   - one `key = value` per line.
3. Write it **atomically**: write `config.tmp` in the same directory, `fsync`, rename over
   `config`. Mode `0640`, owner root.
4. Changing limits on a **running** container: rewrite the file *and* apply live via
   `lxc-cgroup` (through `run-command-safely`). If the live apply fails, report it; the file
   change still takes effect on next start.

## Required lines (every container)

```
lxc.idmap = u 0 100000 65536
lxc.idmap = g 0 100000 65536
lxc.apparmor.profile = generated
lxc.apparmor.allow_nesting = 0
lxc.cgroup2.memory.max = <bytes>
lxc.cgroup2.memory.swap.max = <bytes>
lxc.cgroup2.cpu.max = <quota> 100000
lxc.cgroup2.pids.max = <n, default 4096>
lxc.start.auto = <0|1>
lxc.net.0.type = veth
lxc.net.0.link = <validated bridge name>
lxc.net.0.flags = up
```

- `cpu.max` quota = cores × 100000 (e.g. 4 cores → `400000 100000`). "Unlimited" is `max 100000`.
- Memory values are bytes, computed from MiB with checked arithmetic (no overflow).

## Forbidden — never generate these

- `lxc.apparmor.profile = unconfined` (or any profile other than `generated`).
- Removing ID mapping, or any idmap other than the one above (privileged containers are out of v1).
- `lxc.cap.drop` empty / `lxc.cap.keep` lists.
- `lxc.cgroup2.devices.allow` entries other than those the NVIDIA hook needs.
- `lxc.hook.*` other than the NVIDIA hook constant path below.
- `lxc.init.cmd`, `lxc.execute.cmd`, `lxc.environment` values taken from user input.
- `lxc.include` of any path.
- Any value containing `\n`, `\r`, `\0` or `#` (validators reject them; renderer asserts it).

## Nesting (Podman inside a container)

Only when `nesting = true`: set `lxc.apparmor.allow_nesting = 1` and the nesting mount lines
specified in `SPEC.md` §5.3. Nesting widens the attack surface — it is off by default and only the
app catalog's `app-runner` template or an explicit user choice turns it on.

## Volumes / mount entries

```
lxc.mount.entry = <host_path> <target> none bind,create=dir 0 0
```

- `host_path` must be a dataset/directory created by Arilbox under the configured volume root;
  canonicalize and check it stays inside that root (no symlink escape).
- `target` is relative (no leading `/`), has no `..` segments, no spaces, and matches the
  validated mount path from the request.
- Ownership: volumes are chowned to the mapped root (`100000:100000`) so the container can write.

## GPU (only when `gpu = true` and `capabilities.nvidia = true`)

Exactly these lines from `SPEC.md` §10, nothing more:

```
lxc.hook.mount = /usr/share/lxc/hooks/nvidia
lxc.environment = NVIDIA_VISIBLE_DEVICES=<all | validated index list>
lxc.environment = NVIDIA_DRIVER_CAPABILITIES=compute,utility
```

If GPU is requested but `capabilities.nvidia = false`, fail validation (409) — don't write a
config that silently lacks the GPU.

## Tests (required for every change)

- **Golden tests:** for each scenario (default, limits changed, nesting, volumes, GPU), compare
  `render_config` output to a file in `crates/arilxc/tests/fixtures/*.conf`. Update fixtures
  deliberately and review the diff.
- **Determinism test:** rendering twice gives identical output.
- **Rejection tests:** hostile names/paths (`../etc`, `/etc/shadow`, `a\nlxc.include=/x`,
  `a b`) are rejected before rendering.
- **Integration (feature `lxc-integration`):** create `arilbox-test-*` container with the
  config, start it, check `cat /proc/self/uid_map` inside shows the mapping, destroy it.

## Checklist before finishing

- [ ] Change matches `SPEC.md`; spec updated in the same change if the design changed.
- [ ] No forbidden lines can be produced (add a test if a new field was added).
- [ ] Golden fixtures updated and reviewed.
- [ ] Atomic write path used.
