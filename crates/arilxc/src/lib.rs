//! Container backend for Arilbox (`SPEC.md` §5.1).
//!
//! All container operations go through [`ContainerBackend`]. v1 ships two implementations:
//! `LxcCli` (M2, drives the `lxc-*` tools through `arilbox_core::exec`) and [`MockBackend`]
//! (in-memory, for API and job tests).

mod backend;
pub mod config;
mod error;
mod fsutil;
pub mod lxc_cli;
mod mock;
pub mod template;
mod types;

pub use backend::ContainerBackend;
pub use error::{Error, Result};
pub use lxc_cli::LxcCli;
pub use mock::MockBackend;
pub use types::{
    ContainerInfo, ContainerState, CreateSpec, ExecOutput, Limits, NetworkMode, NetworkSpec,
    SnapshotInfo,
};
