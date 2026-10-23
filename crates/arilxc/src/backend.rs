//! The [`ContainerBackend`] trait.

use std::time::Duration;

use arilbox_core::{ContainerName, SnapshotName};

use crate::error::Result;
use crate::types::{ContainerInfo, CreateSpec, ExecOutput, Limits, SnapshotInfo};

/// Operations on system containers. API code never touches LXC except through this trait.
#[async_trait::async_trait]
pub trait ContainerBackend: Send + Sync {
    /// Lists all containers.
    async fn list(&self) -> Result<Vec<ContainerInfo>>;
    /// Details of one container.
    async fn info(&self, name: &ContainerName) -> Result<ContainerInfo>;
    /// Creates a stopped container.
    async fn create(&self, spec: &CreateSpec) -> Result<()>;
    /// Starts a stopped container.
    async fn start(&self, name: &ContainerName) -> Result<()>;
    /// Shuts a container down cleanly, killing it after `timeout`.
    async fn stop(&self, name: &ContainerName, timeout: Duration) -> Result<()>;
    /// Kills a container immediately.
    async fn kill(&self, name: &ContainerName) -> Result<()>;
    /// Deletes a stopped container and its rootfs.
    async fn destroy(&self, name: &ContainerName) -> Result<()>;
    /// Applies resource limits.
    async fn set_limits(&self, name: &ContainerName, limits: &Limits) -> Result<()>;
    /// Snapshots the rootfs.
    async fn snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    /// Restores a stopped container to a snapshot.
    async fn restore(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    /// Deletes a snapshot.
    async fn delete_snapshot(&self, name: &ContainerName, snap: &SnapshotName) -> Result<()>;
    /// Lists snapshots, oldest first.
    async fn list_snapshots(&self, name: &ContainerName) -> Result<Vec<SnapshotInfo>>;
    /// Runs a command inside a running container (argument vector, no shell).
    async fn exec(&self, name: &ContainerName, cmd: &[String]) -> Result<ExecOutput>;
}
