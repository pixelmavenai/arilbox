//! Validated identifiers.
//!
//! Every identifier that reaches LXC, ZFS, the network stack or a file path is one of these
//! newtypes. Validation rejects; it never sanitizes. Once constructed, a value is known to be
//! safe to use as a single command-line argument or path component.

use std::ffi::OsStr;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Names that may not be used for containers (`SPEC.md` §5.2).
pub const RESERVED_CONTAINER_NAMES: &[&str] = &["arilbox", "host", "default"];

macro_rules! name_type {
    ($(#[$meta:meta])* $ty:ident, $kind:literal, $validate:path) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $ty(String);

        impl $ty {
            /// Validates `value` and wraps it.
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                $validate(&value).map_err(|reason| Error::InvalidName { kind: $kind, reason })?;
                Ok(Self(value))
            }

            /// The validated value.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The validated value, for use as a command argument.
            pub fn as_os_str(&self) -> &OsStr {
                OsStr::new(&self.0)
            }
        }

        impl FromStr for $ty {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = Error;
            fn try_from(s: String) -> Result<Self> {
                Self::new(s)
            }
        }

        impl From<$ty> for String {
            fn from(v: $ty) -> String {
                v.0
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// A container name: `^[a-z][a-z0-9-]{0,62}$`, not ending in `-`, not reserved.
    ContainerName,
    "container name",
    validate_container_name
);

name_type!(
    /// A snapshot name: `^[a-z0-9][a-z0-9._-]{0,62}$`.
    SnapshotName,
    "snapshot name",
    validate_snapshot_name
);

name_type!(
    /// A storage pool name from the config: same rules as a container name, without the
    /// reserved list.
    StorageName,
    "storage name",
    validate_storage_name
);

name_type!(
    /// A Linux bridge interface name: `^[a-z][a-z0-9_-]{0,14}$` (fits `IFNAMSIZ`).
    BridgeName,
    "bridge name",
    validate_bridge_name
);

name_type!(
    /// A ZFS dataset path such as `tank/arilbox`. Components are `[A-Za-z0-9][A-Za-z0-9_.:-]*`,
    /// separated by single `/`, at most 255 bytes in total.
    DatasetPath,
    "dataset path",
    validate_dataset_path
);

fn is_lower_alnum(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// `^[a-z][a-z0-9-]{0,62}$`, not ending in `-`.
fn validate_dns_label(value: &str) -> Result<(), &'static str> {
    let mut chars = value.chars();
    match chars.next() {
        None => return Err("must not be empty"),
        Some(c) if !c.is_ascii_lowercase() => return Err("must start with a lowercase letter"),
        Some(_) => {}
    }
    if value.len() > 63 {
        return Err("must be at most 63 characters");
    }
    if !chars.all(|c| is_lower_alnum(c) || c == '-') {
        return Err("may only contain lowercase letters, digits and '-'");
    }
    if value.ends_with('-') {
        return Err("must not end with '-'");
    }
    Ok(())
}

fn validate_container_name(value: &str) -> Result<(), &'static str> {
    validate_dns_label(value)?;
    if RESERVED_CONTAINER_NAMES.contains(&value) {
        return Err("is a reserved name");
    }
    Ok(())
}

fn validate_storage_name(value: &str) -> Result<(), &'static str> {
    validate_dns_label(value)
}

fn validate_snapshot_name(value: &str) -> Result<(), &'static str> {
    let mut chars = value.chars();
    match chars.next() {
        None => return Err("must not be empty"),
        Some(c) if !is_lower_alnum(c) => {
            return Err("must start with a lowercase letter or digit");
        }
        Some(_) => {}
    }
    if value.len() > 63 {
        return Err("must be at most 63 characters");
    }
    if !chars.all(|c| is_lower_alnum(c) || matches!(c, '.' | '_' | '-')) {
        return Err("may only contain lowercase letters, digits, '.', '_' and '-'");
    }
    Ok(())
}

fn validate_bridge_name(value: &str) -> Result<(), &'static str> {
    let mut chars = value.chars();
    match chars.next() {
        None => return Err("must not be empty"),
        Some(c) if !c.is_ascii_lowercase() => return Err("must start with a lowercase letter"),
        Some(_) => {}
    }
    if value.len() > 15 {
        return Err("must be at most 15 characters");
    }
    if !chars.all(|c| is_lower_alnum(c) || matches!(c, '_' | '-')) {
        return Err("may only contain lowercase letters, digits, '_' and '-'");
    }
    Ok(())
}

fn validate_dataset_path(value: &str) -> Result<(), &'static str> {
    if value.is_empty() {
        return Err("must not be empty");
    }
    if value.len() > 255 {
        return Err("must be at most 255 characters");
    }
    for component in value.split('/') {
        let mut chars = component.chars();
        match chars.next() {
            None => return Err("must not contain empty components"),
            Some(c) if !c.is_ascii_alphanumeric() => {
                return Err("components must start with a letter or digit");
            }
            Some(_) => {}
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')) {
            return Err("components may only contain letters, digits, '_', '.', ':' and '-'");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values that must never become a command argument or path component.
    const HOSTILE: &[&str] = &[
        "",
        "-rf",
        "--help",
        "a;rm -rf /",
        "../x",
        "a/../b",
        "a\nb",
        "a\nlxc.include=/x",
        "a b",
        "/etc/shadow",
        "a\0b",
        "$(id)",
        "`id`",
        "é",
    ];

    #[test]
    fn container_name_accepts_valid() {
        for ok in ["a", "web", "ai-01", "x1", &format!("a{}", "b".repeat(62))] {
            assert!(ContainerName::new(ok).is_ok(), "{ok:?} should be valid");
        }
    }

    #[test]
    fn container_name_rejects_invalid() {
        let too_long = format!("a{}", "b".repeat(63));
        for bad in ["1abc", "Web", "web-", "we_b", "we.b", &too_long] {
            assert!(
                ContainerName::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn container_name_rejects_reserved() {
        for bad in RESERVED_CONTAINER_NAMES {
            assert!(ContainerName::new(*bad).is_err());
        }
        // Reserved names are fine for other kinds.
        assert!(StorageName::new("default").is_ok());
    }

    #[test]
    fn snapshot_name_rules() {
        for ok in ["0", "snap-1", "before.update_2", "2026-10-23"] {
            assert!(SnapshotName::new(ok).is_ok(), "{ok:?} should be valid");
        }
        for bad in [".", "..", ".hidden", "-x", "_x", "Snap", "a/b"] {
            assert!(
                SnapshotName::new(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn bridge_name_rules() {
        for ok in ["arilbox0", "vmbr0", "br-lan", "a"] {
            assert!(BridgeName::new(ok).is_ok(), "{ok:?} should be valid");
        }
        for bad in ["0br", "Br0", "br.0", "abcdefghijklmnop"] {
            assert!(BridgeName::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn dataset_path_rules() {
        for ok in [
            "tank",
            "tank/arilbox",
            "rpool/arilbox/test",
            "pool/a.b:c_d-e",
        ] {
            assert!(DatasetPath::new(ok).is_ok(), "{ok:?} should be valid");
        }
        for bad in [
            "/tank",
            "tank/",
            "tank//x",
            "tank/.x",
            "tank/..",
            "tank/-x",
            "tank@snap",
        ] {
            assert!(DatasetPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn hostile_values_rejected_by_every_type() {
        for bad in HOSTILE {
            assert!(ContainerName::new(*bad).is_err(), "container {bad:?}");
            assert!(SnapshotName::new(*bad).is_err(), "snapshot {bad:?}");
            assert!(StorageName::new(*bad).is_err(), "storage {bad:?}");
            assert!(BridgeName::new(*bad).is_err(), "bridge {bad:?}");
            assert!(DatasetPath::new(*bad).is_err(), "dataset {bad:?}");
        }
    }

    #[test]
    fn error_does_not_echo_value() {
        let err = ContainerName::new("evil\nvalue").unwrap_err().to_string();
        assert!(!err.contains("evil"), "{err}");
    }

    #[test]
    fn serde_validates() {
        let ok: ContainerName = serde_json::from_str("\"web\"").unwrap();
        assert_eq!(ok.as_str(), "web");
        assert_eq!(serde_json::to_string(&ok).unwrap(), "\"web\"");
        assert!(serde_json::from_str::<ContainerName>("\"../x\"").is_err());
    }
}
