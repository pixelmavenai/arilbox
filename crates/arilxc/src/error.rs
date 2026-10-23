//! Error type for `arilxc`.

use arilbox_core::{ContainerName, SnapshotName};

use crate::types::ContainerState;

/// Errors from a container backend.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No container with this name.
    #[error("container '{0}' does not exist")]
    NotFound(ContainerName),
    /// A container with this name already exists.
    #[error("container '{0}' already exists")]
    Exists(ContainerName),
    /// The operation is not allowed in the container's current state.
    #[error("container '{name}' is {state}")]
    WrongState {
        /// Container.
        name: ContainerName,
        /// Its current state.
        state: ContainerState,
    },
    /// No snapshot with this name.
    #[error("snapshot '{snap}' of container '{name}' does not exist")]
    SnapshotNotFound {
        /// Container.
        name: ContainerName,
        /// Snapshot.
        snap: SnapshotName,
    },
    /// A snapshot with this name already exists.
    #[error("snapshot '{snap}' of container '{name}' already exists")]
    SnapshotExists {
        /// Container.
        name: ContainerName,
        /// Snapshot.
        snap: SnapshotName,
    },
    /// The request uses something this backend or build does not support yet.
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    /// The backend failed. Details are for logs, not API clients.
    #[error("container backend error: {0}")]
    Backend(String),
}

/// Result alias for `arilxc`.
pub type Result<T, E = Error> = std::result::Result<T, E>;
