//! Daemon configuration (`/etc/arilbox/config.toml`, `SPEC.md` §16).
//!
//! Defaults are the production values from the spec. Development setups override them in
//! `packaging/dev-config.toml`; dev paths never appear in code.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::name::{BridgeName, DatasetPath, StorageName};

/// Default location of the config file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/arilbox/config.toml";

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// HTTP(S) listener settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Filesystem locations.
    #[serde(default)]
    pub paths: PathsConfig,
    /// Container backend.
    #[serde(default)]
    pub containers: ContainersConfig,
    /// Managed bridge network.
    #[serde(default)]
    pub network: NetworkConfig,
    /// Storage pools.
    #[serde(default)]
    pub storage: Vec<StoragePool>,
    /// Defaults for new containers.
    #[serde(default)]
    pub defaults: DefaultsConfig,
}

/// `[server]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    /// Listen address.
    pub bind: SocketAddr,
    /// PEM certificate chain. Generated (self-signed) on first start if missing.
    pub tls_cert: PathBuf,
    /// PEM private key. Generated on first start if missing.
    pub tls_key: PathBuf,
    /// Serve HTTPS. May only be `false` together with `dev_mode = true`.
    pub tls: bool,
    /// Development mode: human-readable logs and plain HTTP allowed.
    pub dev_mode: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 8443)),
            tls_cert: PathBuf::from("/etc/arilbox/tls/cert.pem"),
            tls_key: PathBuf::from("/etc/arilbox/tls/key.pem"),
            tls: true,
            dev_mode: false,
        }
    }
}

/// `[paths]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PathsConfig {
    /// SQLite database and job logs.
    pub data_dir: PathBuf,
    /// Built-in app catalog.
    pub catalog_dir: PathBuf,
    /// Built web UI (`web/dist`), served at `/`.
    pub web_dir: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/arilbox"),
            catalog_dir: PathBuf::from("/usr/share/arilbox/catalog"),
            web_dir: PathBuf::from("/usr/share/arilbox/web"),
        }
    }
}

/// Which [`ContainersConfig::backend`] implementation the daemon uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Real LXC through the `lxc-*` tools.
    #[default]
    Lxc,
    /// In-memory mock: no host changes. Only allowed with `server.dev_mode = true`.
    Mock,
}

/// `[containers]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ContainersConfig {
    /// Backend implementation.
    pub backend: BackendKind,
    /// LXC container directory (`lxc-* -P`).
    pub lxc_path: PathBuf,
}

impl Default for ContainersConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::Lxc,
            lxc_path: PathBuf::from("/var/lib/lxc"),
        }
    }
}

/// `[network]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetworkConfig {
    /// Managed bridge name.
    pub bridge: BridgeName,
    /// Bridge address and prefix, e.g. `10.88.0.1/24`.
    pub subnet: Ipv4Cidr,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bridge: BridgeName::from_static("arilbox0"),
            subnet: Ipv4Cidr {
                addr: Ipv4Addr::new(10, 88, 0, 1),
                prefix: 24,
            },
        }
    }
}

impl BridgeName {
    /// For compile-time constants that are known to be valid.
    fn from_static(value: &'static str) -> Self {
        // Invariant: only called with literals covered by `default_bridge_is_valid`.
        #[allow(clippy::expect_used)]
        BridgeName::new(value).expect("static bridge name is valid")
    }
}

/// An IPv4 address with prefix length, e.g. `10.88.0.1/24`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Ipv4Cidr {
    /// Host address on the bridge.
    pub addr: Ipv4Addr,
    /// Prefix length, 8..=30.
    pub prefix: u8,
}

impl TryFrom<String> for Ipv4Cidr {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        let invalid = || Error::Config("subnet must look like 10.88.0.1/24".into());
        let (addr, prefix) = value.split_once('/').ok_or_else(invalid)?;
        let addr: Ipv4Addr = addr.parse().map_err(|_| invalid())?;
        let prefix: u8 = prefix.parse().map_err(|_| invalid())?;
        if !(8..=30).contains(&prefix) {
            return Err(Error::Config(
                "subnet prefix must be between 8 and 30".into(),
            ));
        }
        Ok(Self { addr, prefix })
    }
}

impl std::fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// A `[[storage]]` entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoragePool {
    /// ZFS dataset root.
    Zfs {
        /// Pool name used in the API.
        name: StorageName,
        /// Root dataset, created if missing.
        dataset: DatasetPath,
    },
    /// Plain directory root.
    Dir {
        /// Pool name used in the API.
        name: StorageName,
        /// Absolute directory path.
        path: PathBuf,
    },
}

impl StoragePool {
    /// The pool's name.
    pub fn name(&self) -> &StorageName {
        match self {
            StoragePool::Zfs { name, .. } | StoragePool::Dir { name, .. } => name,
        }
    }
}

/// `[defaults]` section.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct DefaultsConfig {
    /// Default storage pool for new containers; must name a configured pool.
    pub storage: Option<StorageName>,
    /// Default template.
    pub template: TemplateRef,
}

/// A distribution template, e.g. `debian` / `13`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateRef {
    /// Distribution, e.g. `debian`.
    pub distro: String,
    /// Release, e.g. `13`.
    pub release: String,
}

impl Default for TemplateRef {
    fn default() -> Self {
        Self {
            distro: "debian".into(),
            release: "13".into(),
        }
    }
}

impl TemplateRef {
    /// Checks both parts are `[a-z0-9][a-z0-9.]*` (max 32), so they are safe as arguments.
    pub fn validate(&self) -> Result<()> {
        fn ok(s: &str) -> bool {
            let mut chars = s.chars();
            s.len() <= 32
                && chars
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.')
        }
        if ok(&self.distro) && ok(&self.release) {
            Ok(())
        } else {
            Err(Error::InvalidName {
                kind: "template",
                reason: "distro and release may only contain lowercase letters, digits and '.'",
            })
        }
    }
}

impl Config {
    /// Parses and validates a config from TOML text.
    pub fn from_toml(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Reads, parses and validates the config file at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            context: format!("reading config {}", path.display()),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Cross-field checks that serde can't express.
    pub fn validate(&self) -> Result<()> {
        if !self.server.tls && !self.server.dev_mode {
            return Err(Error::Config(
                "server.tls = false is only allowed with server.dev_mode = true".into(),
            ));
        }

        if self.containers.backend == BackendKind::Mock && !self.server.dev_mode {
            return Err(Error::Config(
                "containers.backend = \"mock\" is only allowed with server.dev_mode = true".into(),
            ));
        }
        if !self.containers.lxc_path.is_absolute() {
            return Err(Error::Config("containers.lxc_path must be absolute".into()));
        }

        let mut names = HashSet::new();
        for pool in &self.storage {
            if !names.insert(pool.name()) {
                return Err(Error::Config(format!(
                    "duplicate storage pool name '{}'",
                    pool.name()
                )));
            }
            if let StoragePool::Dir { path, name } = pool
                && !path.is_absolute()
            {
                return Err(Error::Config(format!(
                    "storage pool '{name}': path must be absolute"
                )));
            }
        }

        if let Some(default) = &self.defaults.storage
            && !names.contains(default)
        {
            return Err(Error::Config(format!(
                "defaults.storage '{default}' is not a configured storage pool"
            )));
        }

        self.defaults.template.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bridge_is_valid() {
        assert_eq!(NetworkConfig::default().bridge.as_str(), "arilbox0");
    }

    #[test]
    fn empty_config_uses_spec_defaults() {
        let c = Config::from_toml("").unwrap();
        assert_eq!(c.server.bind, "0.0.0.0:8443".parse().unwrap());
        assert!(c.server.tls);
        assert!(!c.server.dev_mode);
        assert_eq!(c.paths.data_dir, PathBuf::from("/var/lib/arilbox"));
        assert_eq!(c.paths.web_dir, PathBuf::from("/usr/share/arilbox/web"));
        assert_eq!(c.network.subnet.to_string(), "10.88.0.1/24");
        assert_eq!(c.defaults.template, TemplateRef::default());
    }

    #[test]
    fn parses_spec_example() {
        let c = Config::from_toml(
            r#"
            [server]
            bind = "0.0.0.0:8443"
            tls_cert = "/etc/arilbox/tls/cert.pem"
            tls_key = "/etc/arilbox/tls/key.pem"

            [paths]
            data_dir = "/var/lib/arilbox"
            catalog_dir = "/usr/share/arilbox/catalog"
            web_dir = "/usr/share/arilbox/web"

            [network]
            bridge = "arilbox0"
            subnet = "10.88.0.1/24"

            [[storage]]
            name = "local-zfs"
            type = "zfs"
            dataset = "rpool/arilbox"

            [[storage]]
            name = "local-dir"
            type = "dir"
            path = "/var/lib/arilbox/storage"

            [defaults]
            storage = "local-zfs"
            template = { distro = "debian", release = "13" }
            "#,
        )
        .unwrap();
        assert_eq!(c.storage.len(), 2);
        assert!(
            matches!(&c.storage[0], StoragePool::Zfs { dataset, .. } if dataset.as_str() == "rpool/arilbox")
        );
        assert_eq!(c.defaults.storage.unwrap().as_str(), "local-zfs");
    }

    #[test]
    fn dev_configs_in_repo_are_valid() {
        let c = Config::from_toml(include_str!("../../../packaging/dev-config.toml")).unwrap();
        assert!(c.server.dev_mode);
        assert_eq!(c.containers.backend, BackendKind::Lxc);
        let c = Config::from_toml(include_str!("../../../packaging/dev-config-mock.toml")).unwrap();
        assert_eq!(c.containers.backend, BackendKind::Mock);
    }

    #[test]
    fn rejects_plain_http_outside_dev_mode() {
        let err = Config::from_toml("[server]\ntls = false").unwrap_err();
        assert!(err.to_string().contains("dev_mode"), "{err}");
        assert!(Config::from_toml("[server]\ntls = false\ndev_mode = true").is_ok());
    }

    #[test]
    fn mock_backend_only_in_dev_mode() {
        assert!(Config::from_toml("[containers]\nbackend = \"mock\"").is_err());
        let c = Config::from_toml("[server]\ndev_mode = true\n[containers]\nbackend = \"mock\"")
            .unwrap();
        assert_eq!(c.containers.backend, BackendKind::Mock);
        assert_eq!(
            Config::from_toml("").unwrap().containers.backend,
            BackendKind::Lxc
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        assert!(Config::from_toml("[server]\nbindd = \"0.0.0.0:1\"").is_err());
        assert!(Config::from_toml("typo = 1").is_err());
    }

    #[test]
    fn rejects_bad_values() {
        for bad in [
            "[network]\nbridge = \"Bad Name\"",
            "[network]\nsubnet = \"10.88.0.1\"",
            "[network]\nsubnet = \"10.88.0.1/31\"",
            "[[storage]]\nname = \"x\"\ntype = \"zfs\"\ndataset = \"/abs\"",
            "[[storage]]\nname = \"x\"\ntype = \"dir\"\npath = \"relative\"",
            "[[storage]]\nname = \"x\"\ntype = \"nfs\"\npath = \"/x\"",
            "[defaults]\nstorage = \"missing\"",
            "[defaults]\ntemplate = { distro = \"../x\", release = \"13\" }",
        ] {
            assert!(Config::from_toml(bad).is_err(), "should reject: {bad}");
        }
    }

    #[test]
    fn rejects_duplicate_storage_names() {
        let text = "[[storage]]\nname = \"a\"\ntype = \"dir\"\npath = \"/a\"\n\
                    [[storage]]\nname = \"a\"\ntype = \"dir\"\npath = \"/b\"";
        assert!(Config::from_toml(text).is_err());
    }
}
