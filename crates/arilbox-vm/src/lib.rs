//! Virtual machine backend (`SPEC.md` §13).
//!
//! VMs are designed but not part of v1. This crate defines the [`VmBackend`] trait so the rest
//! of the platform can be written against it, and [`UnsupportedVmBackend`], which every v1
//! build uses. The future implementation runs Cloud Hypervisor; never QEMU.

use serde::Serialize;

/// VM names follow the container naming rules.
pub type VmName = arilbox_core::ContainerName;

/// Errors from a VM backend.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// VMs are not available in this build or on this host.
    #[error("virtual machines are not supported")]
    Unsupported,
}

/// Result alias for `arilbox-vm`.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Summary of a VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VmInfo {
    /// VM name.
    pub name: VmName,
}

/// Operations on virtual machines. Mirrors `arilxc::ContainerBackend`; extended when VMs are
/// implemented (M7).
#[async_trait::async_trait]
pub trait VmBackend: Send + Sync {
    /// Lists VMs.
    async fn list(&self) -> Result<Vec<VmInfo>>;
    /// Starts a VM.
    async fn start(&self, name: &VmName) -> Result<()>;
    /// Stops a VM.
    async fn stop(&self, name: &VmName) -> Result<()>;
    /// Destroys a VM and its disks.
    async fn destroy(&self, name: &VmName) -> Result<()>;
}

/// The v1 backend: every operation returns [`Error::Unsupported`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UnsupportedVmBackend;

#[async_trait::async_trait]
impl VmBackend for UnsupportedVmBackend {
    async fn list(&self) -> Result<Vec<VmInfo>> {
        Err(Error::Unsupported)
    }
    async fn start(&self, _name: &VmName) -> Result<()> {
        Err(Error::Unsupported)
    }
    async fn stop(&self, _name: &VmName) -> Result<()> {
        Err(Error::Unsupported)
    }
    async fn destroy(&self, _name: &VmName) -> Result<()> {
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn everything_is_unsupported() {
        let b = UnsupportedVmBackend;
        let name = VmName::new("vm1").unwrap();
        assert!(matches!(b.list().await, Err(Error::Unsupported)));
        assert!(matches!(b.start(&name).await, Err(Error::Unsupported)));
        assert!(matches!(b.stop(&name).await, Err(Error::Unsupported)));
        assert!(matches!(b.destroy(&name).await, Err(Error::Unsupported)));
    }
}
