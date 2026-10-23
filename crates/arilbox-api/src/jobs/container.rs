//! Container jobs.
//!
//! Step ids (stable, shown in the UI and stored in job rows):
//!
//! | Step id | Run | Compensate |
//! |---|---|---|
//! | `create_container` | create rootfs + config (`lxc-create`) | kill if running, destroy |
//! | `start_container` | start | kill (only in `container_create`) |
//! | `wait_for_network` | poll until the container has an IPv4 address | — |
//! | `stop_container` | clean shutdown with timeout; no-op if already stopped | — |
//! | `kill_container` | immediate stop; no-op if already stopped | — |
//! | `destroy_container` | delete container and rootfs | — (cannot be undone) |
//!
//! Jobs: `container_create` = create (+ start + wait when `start` is requested);
//! `container_start` = start + wait; `container_stop` = stop; `container_restart` = stop +
//! start + wait; `container_delete` = (kill when forced) + destroy.

use std::time::Duration;

use arilbox_core::ContainerName;
use arilxc::{ContainerState, CreateSpec};

use super::{JobCtx, JobKind, JobSpec, Step, StepError};

/// Maps backend errors to step errors. Details stay in the log.
fn backend_error(e: arilxc::Error) -> StepError {
    use arilxc::Error as E;
    match e {
        E::NotFound(name) => StepError::new(
            "container_not_found",
            format!("Container '{name}' does not exist"),
        ),
        E::Exists(name) => StepError::new(
            "container_exists",
            format!("Container '{name}' already exists"),
        ),
        E::WrongState { name, state } => StepError::new(
            "container_wrong_state",
            format!("Container '{name}' is {state}"),
        ),
        E::Unsupported(what) => StepError::new("unsupported", format!("Not supported: {what}")),
        E::SnapshotNotFound { .. } | E::SnapshotExists { .. } | E::Backend(_) => {
            tracing::error!(error = %e, "container backend error");
            StepError::new(
                "backend_error",
                "The container backend reported an error; see the server log",
            )
        }
    }
}

struct CreateContainer(CreateSpec);

#[async_trait::async_trait]
impl Step for CreateContainer {
    fn id(&self) -> &'static str {
        "create_container"
    }
    fn label(&self) -> &'static str {
        "Creating container"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        ctx.log(format!(
            "Creating '{}' from {} {}",
            self.0.name, self.0.template.distro, self.0.template.release
        ));
        ctx.containers.create(&self.0).await.map_err(backend_error)
    }
    async fn compensate(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        let name = &self.0.name;
        match ctx.containers.info(name).await {
            Err(arilxc::Error::NotFound(_)) => return Ok(()),
            Ok(info) if info.state != ContainerState::Stopped => {
                // Best effort; destroy reports the real problem if this didn't work.
                let _ = ctx.containers.kill(name).await;
            }
            _ => {}
        }
        ctx.log(format!("Removing '{name}'"));
        match ctx.containers.destroy(name).await {
            Ok(()) | Err(arilxc::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(backend_error(e)),
        }
    }
}

struct StartContainer {
    name: ContainerName,
    /// Kill on rollback (only when this job also created the container).
    undo: bool,
}

#[async_trait::async_trait]
impl Step for StartContainer {
    fn id(&self) -> &'static str {
        "start_container"
    }
    fn label(&self) -> &'static str {
        "Starting container"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        ctx.containers
            .start(&self.name)
            .await
            .map_err(backend_error)
    }
    async fn compensate(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        if !self.undo {
            return Ok(());
        }
        match ctx.containers.kill(&self.name).await {
            Ok(()) | Err(arilxc::Error::WrongState { .. } | arilxc::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(backend_error(e)),
        }
    }
}

struct WaitForNetwork {
    name: ContainerName,
    timeout: Duration,
}

#[async_trait::async_trait]
impl Step for WaitForNetwork {
    fn id(&self) -> &'static str {
        "wait_for_network"
    }
    fn label(&self) -> &'static str {
        "Waiting for network"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let interval = (self.timeout / 20).clamp(Duration::from_millis(10), Duration::from_secs(1));
        loop {
            let info = ctx
                .containers
                .info(&self.name)
                .await
                .map_err(backend_error)?;
            if let Some(ip) = info.ipv4.first() {
                ctx.log(format!("Got address {ip}"));
                return Ok(());
            }
            if info.state != ContainerState::Running && info.state != ContainerState::Starting {
                return Err(StepError::new(
                    "container_wrong_state",
                    format!(
                        "Container '{}' is {} instead of running",
                        self.name, info.state
                    ),
                ));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(StepError::new(
                    "network_timeout",
                    format!(
                        "Container '{}' got no IPv4 address within {} s",
                        self.name,
                        self.timeout.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(interval).await;
        }
    }
}

struct StopContainer {
    name: ContainerName,
    timeout: Duration,
}

#[async_trait::async_trait]
impl Step for StopContainer {
    fn id(&self) -> &'static str {
        "stop_container"
    }
    fn label(&self) -> &'static str {
        "Stopping container"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        let info = ctx
            .containers
            .info(&self.name)
            .await
            .map_err(backend_error)?;
        if info.state == ContainerState::Stopped {
            ctx.log("Already stopped");
            return Ok(());
        }
        ctx.containers
            .stop(&self.name, self.timeout)
            .await
            .map_err(backend_error)
    }
}

struct KillContainer(ContainerName);

#[async_trait::async_trait]
impl Step for KillContainer {
    fn id(&self) -> &'static str {
        "kill_container"
    }
    fn label(&self) -> &'static str {
        "Stopping container immediately"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        let info = ctx.containers.info(&self.0).await.map_err(backend_error)?;
        if info.state == ContainerState::Stopped {
            return Ok(());
        }
        ctx.containers.kill(&self.0).await.map_err(backend_error)
    }
}

struct DestroyContainer(ContainerName);

#[async_trait::async_trait]
impl Step for DestroyContainer {
    fn id(&self) -> &'static str {
        "destroy_container"
    }
    fn label(&self) -> &'static str {
        "Deleting container"
    }
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
        ctx.containers.destroy(&self.0).await.map_err(backend_error)
    }
}

/// `container_create`.
pub fn create_job(spec: CreateSpec, start: bool, network_timeout: Duration) -> JobSpec {
    let name = spec.name.clone();
    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(CreateContainer(spec))];
    if start {
        steps.push(Box::new(StartContainer {
            name: name.clone(),
            undo: true,
        }));
        steps.push(Box::new(WaitForNetwork {
            name: name.clone(),
            timeout: network_timeout,
        }));
    }
    JobSpec {
        kind: JobKind::ContainerCreate,
        target: name.to_string(),
        steps,
    }
}

/// `container_start`.
pub fn start_job(name: ContainerName, network_timeout: Duration) -> JobSpec {
    JobSpec {
        kind: JobKind::ContainerStart,
        target: name.to_string(),
        steps: vec![
            Box::new(StartContainer {
                name: name.clone(),
                undo: false,
            }),
            Box::new(WaitForNetwork {
                name,
                timeout: network_timeout,
            }),
        ],
    }
}

/// `container_stop`.
pub fn stop_job(name: ContainerName, timeout: Duration) -> JobSpec {
    JobSpec {
        kind: JobKind::ContainerStop,
        target: name.to_string(),
        steps: vec![Box::new(StopContainer { name, timeout })],
    }
}

/// `container_restart`.
pub fn restart_job(name: ContainerName, timeout: Duration, network_timeout: Duration) -> JobSpec {
    JobSpec {
        kind: JobKind::ContainerRestart,
        target: name.to_string(),
        steps: vec![
            Box::new(StopContainer {
                name: name.clone(),
                timeout,
            }),
            Box::new(StartContainer {
                name: name.clone(),
                undo: false,
            }),
            Box::new(WaitForNetwork {
                name,
                timeout: network_timeout,
            }),
        ],
    }
}

/// `container_delete`.
pub fn delete_job(name: ContainerName, force: bool) -> JobSpec {
    let mut steps: Vec<Box<dyn Step>> = Vec::new();
    if force {
        steps.push(Box::new(KillContainer(name.clone())));
    }
    steps.push(Box::new(DestroyContainer(name.clone())));
    JobSpec {
        kind: JobKind::ContainerDelete,
        target: name.to_string(),
        steps,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arilbox_core::config::TemplateRef;
    use arilbox_core::{BridgeName, StorageName};
    use arilxc::{ContainerBackend, Limits, MockBackend, NetworkSpec};

    use super::*;
    use crate::jobs::tests::wait;
    use crate::jobs::{JobStatus, Jobs, StepStatus};

    fn name(s: &str) -> ContainerName {
        ContainerName::new(s).unwrap()
    }

    fn spec(n: &str) -> CreateSpec {
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

    async fn setup() -> (Jobs, Arc<MockBackend>) {
        let backend = Arc::new(MockBackend::new());
        let mut jobs = Jobs::new(crate::db::memory().await.unwrap(), backend.clone());
        jobs.network_timeout = Duration::from_millis(200);
        (jobs, backend)
    }

    const NET: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn create_and_start() {
        let (jobs, backend) = setup().await;
        let id = jobs
            .submit(create_job(spec("web"), true, NET))
            .await
            .unwrap();
        let job = wait(&jobs, id).await;
        assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
        let ids: Vec<_> = job.steps.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["create_container", "start_container", "wait_for_network"]
        );
        let info = backend.info(&name("web")).await.unwrap();
        assert_eq!(info.state, ContainerState::Running);
        assert!(
            job.steps[2].log[0]
                .message
                .starts_with("Got address 10.88.0.")
        );
    }

    #[tokio::test]
    async fn create_without_start() {
        let (jobs, backend) = setup().await;
        let job = wait(
            &jobs,
            jobs.submit(create_job(spec("web"), false, NET))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(job.steps.len(), 1);
        assert_eq!(
            backend.info(&name("web")).await.unwrap().state,
            ContainerState::Stopped
        );
    }

    #[tokio::test]
    async fn create_failure_leaves_nothing() {
        let (jobs, backend) = setup().await;
        backend.fail_on("create");
        let job = wait(
            &jobs,
            jobs.submit(create_job(spec("web"), true, NET))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_ref().unwrap().code, "backend_error");
        assert!(backend.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_failure_rolls_back_create() {
        let (jobs, backend) = setup().await;
        backend.fail_on("start");
        let job = wait(
            &jobs,
            jobs.submit(create_job(spec("web"), true, NET))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.steps[0].status, StepStatus::Compensated);
        assert!(backend.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn network_timeout_rolls_back_start_and_create() {
        let (jobs, backend) = setup().await;
        backend.fail_on("network");
        let job = wait(
            &jobs,
            jobs.submit(create_job(spec("web"), true, NET))
                .await
                .unwrap(),
        )
        .await;
        let err = job.error.unwrap();
        assert_eq!(
            (err.code.as_str(), err.step.as_deref()),
            ("network_timeout", Some("wait_for_network"))
        );
        assert_eq!(job.steps[1].status, StepStatus::Compensated);
        assert_eq!(job.steps[0].status, StepStatus::Compensated);
        assert!(backend.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_rollback_is_reported() {
        let (jobs, backend) = setup().await;
        backend.fail_on("network");
        backend.fail_on("kill");
        backend.fail_on("destroy");
        let job = wait(
            &jobs,
            jobs.submit(create_job(spec("web"), true, NET))
                .await
                .unwrap(),
        )
        .await;
        assert!(job.error.unwrap().rollback_incomplete);
        assert_eq!(job.steps[0].status, StepStatus::CompensationFailed);
    }

    #[tokio::test]
    async fn start_stop_restart_delete() {
        let (jobs, backend) = setup().await;
        let n = name("web");
        backend.create(&spec("web")).await.unwrap();

        let job = wait(&jobs, jobs.submit(start_job(n.clone(), NET)).await.unwrap()).await;
        assert_eq!(job.status, JobStatus::Succeeded);

        let job = wait(
            &jobs,
            jobs.submit(restart_job(n.clone(), NET, NET)).await.unwrap(),
        )
        .await;
        assert_eq!(job.status, JobStatus::Succeeded);
        assert_eq!(
            backend.info(&n).await.unwrap().state,
            ContainerState::Running
        );

        let job = wait(&jobs, jobs.submit(stop_job(n.clone(), NET)).await.unwrap()).await;
        assert_eq!(job.status, JobStatus::Succeeded);
        // Stopping again is a no-op, not an error.
        let job = wait(&jobs, jobs.submit(stop_job(n.clone(), NET)).await.unwrap()).await;
        assert_eq!(job.steps[0].log[0].message, "Already stopped");

        let job = wait(
            &jobs,
            jobs.submit(delete_job(n.clone(), false)).await.unwrap(),
        )
        .await;
        assert_eq!(job.status, JobStatus::Succeeded);
        assert!(backend.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn start_job_does_not_stop_container_on_network_timeout() {
        let (jobs, backend) = setup().await;
        let n = name("web");
        backend.create(&spec("web")).await.unwrap();
        backend.fail_on("network");
        let job = wait(&jobs, jobs.submit(start_job(n.clone(), NET)).await.unwrap()).await;
        assert_eq!(job.status, JobStatus::Failed);
        // The user asked for it to run; leave it running so they can debug.
        assert_eq!(
            backend.info(&n).await.unwrap().state,
            ContainerState::Running
        );
    }

    #[tokio::test]
    async fn forced_delete_kills_first() {
        let (jobs, backend) = setup().await;
        let n = name("web");
        backend.create(&spec("web")).await.unwrap();
        backend.start(&n).await.unwrap();
        let job = wait(&jobs, jobs.submit(delete_job(n, true)).await.unwrap()).await;
        assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
        assert_eq!(job.steps[0].id, "kill_container");
        assert!(backend.list().await.unwrap().is_empty());
    }
}
