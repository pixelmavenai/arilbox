//! Shared building blocks for Arilbox: validated identifiers, configuration, host probing and
//! the only allowed way to run external programs ([`exec`]).

pub mod config;
pub mod error;
pub mod exec;
pub mod host;
pub mod name;

pub use error::{Error, Result};
pub use name::{BridgeName, ContainerName, DatasetPath, SnapshotName, StorageName};
