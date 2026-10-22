---
name: add-job
description: Use when adding or changing a long-running Arilbox operation (create, delete, start, stop, snapshot, restore, app install/update/uninstall). Defines jobs as named steps with rollback, progress events and failure-path tests.
---

# Adding a job

Anything that can take more than about a second, or that changes several things on the host,
is a **job** (`SPEC.md` §8). The HTTP request validates and returns `202 { "job_id" }`
immediately; the job runner does the work and reports progress.

## Model

```rust
pub struct JobSpec {
    pub kind: JobKind,          // e.g. ContainerCreate, AppInstall
    pub target: Target,         // usually a ContainerName; one active job per target
    pub steps: Vec<Box<dyn Step>>,
}

#[async_trait::async_trait]
pub trait Step: Send + Sync {
    /// Stable machine id, e.g. "create_rootfs". Never changes once shipped.
    fn id(&self) -> &'static str;
    /// English label shown in the UI, e.g. "Creating root filesystem".
    fn label(&self) -> &'static str;
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError>;
    /// Undo this step's effect. Default: nothing to undo.
    async fn compensate(&self, ctx: &mut JobCtx) -> Result<(), StepError> { Ok(()) }
}
```

`JobCtx` gives access to the backends (`ContainerBackend`, storage, net), a logger that
records lines against the current step, and shared data between steps (e.g. a created
dataset path).

## Runner behaviour (already provided — don't re-implement)

- Persists the job (`queued → running → succeeded | failed | cancelled`) and each step's
  start, end and log lines in SQLite.
- Emits SSE events on `/api/v1/jobs/{id}/events`: `step_started`, `log`, `step_finished`,
  `job_finished`.
- **One active job per target**; a second request for the same target gets `409 target_busy`
  at the HTTP layer, before a job is created.
- On a step failure: runs `compensate` for the **already completed** steps in **reverse order**,
  then marks the job `failed` with the failing step id and a stable error code.
- On daemon restart: `running` jobs are marked `failed` with reason `daemon_restarted`.

## Writing steps well

1. **Validate before the job exists.** Name format, conflicts, capacity (RAM, disk, GPU) are
   checked in the HTTP handler and return 400/409. Steps assume valid input.
2. **Small and single-purpose.** One step = one host change (create dataset, write config,
   start container, run health check). This makes compensation simple.
3. **Idempotent where possible.** `run` checks whether its effect already exists; `compensate`
   tolerates partial state (e.g. dataset already gone is success, not an error).
4. **Compensation must not panic or hang.** Log and continue if an undo fails; the job error
   records "rollback incomplete" so the user can see leftovers.
5. **Never compensate data the job didn't create.** Pre-existing volumes are kept (see
   `SPEC.md` §11.3).
6. **Use `run-command-safely`** for any external program and **`lxc-config`** for any config.
7. **Timeouts:** every wait (network up, health check) has a deadline from the spec or manifest.

## Example: container create

| Step id | Run | Compensate |
|---|---|---|
| `create_dataset` | `zfs create tank/arilbox/containers/<name>` | `zfs destroy` it |
| `create_rootfs` | `lxc-create -t download …` | `lxc-destroy` |
| `write_config` | render + atomic write (`lxc-config`) | (removed by `lxc-destroy`) |
| `create_volumes` | create + mount each requested volume | destroy volumes this job created |
| `start` (if requested) | `lxc-start`, wait for IP | `lxc-stop` |

## Tests (required)

Using `MockBackend` / `RecordingExec`:

- [ ] Happy path: all steps run in order, job `succeeded`, events emitted in order.
- [ ] **Failure at each step**: inject a failure at step *n* and assert steps *n-1 … 1* were
      compensated in reverse and nothing else was touched.
- [ ] Compensation failure is recorded and doesn't stop other compensations.
- [ ] Second job on the same target → `409 target_busy`.
- [ ] Integration (feature `lxc-integration`, `arilbox-test-*` names only): real run leaves no
      leftovers after both success-then-delete and forced failure.

## Checklist before finishing

- [ ] Step ids are stable `snake_case` and documented in the job's module doc comment.
- [ ] HTTP handler returns 202 only after validation passes.
- [ ] `SPEC.md` updated if a new job kind or step changes documented behaviour.
