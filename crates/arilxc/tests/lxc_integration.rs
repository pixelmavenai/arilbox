//! Real LXC tests. Run as root on a Debian 13 host with LXC installed:
//!
//! ```text
//! sudo -E cargo test -p arilxc --features lxc-integration -- --test-threads=1
//! ```
//!
//! Only containers named `arilbox-test-*` under the dir pool `/var/lib/arilbox/test` are
//! created, and a drop guard destroys them even if an assertion fails. Containers attach to
//! LXC's own `lxcbr0` (from the `lxc-net` service), which this test does not modify.
#![cfg(feature = "lxc-integration")]
// Test code: panicking on unexpected errors is the point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arilbox_core::config::{StoragePool, TemplateRef};
use arilbox_core::exec::{Program, ProgramPaths, RealExec};
use arilbox_core::{BridgeName, ContainerName, StorageName};
use arilxc::{ContainerBackend, ContainerState, CreateSpec, Limits, LxcCli, NetworkSpec};

const LXC_PATH: &str = "/var/lib/lxc";
const POOL: &str = "/var/lib/arilbox/test";

fn backend() -> LxcCli {
    let paths = ProgramPaths::resolve(&[
        Program::LxcCreate,
        Program::LxcStart,
        Program::LxcStop,
        Program::LxcInfo,
        Program::LxcAttach,
        Program::LxcDestroy,
    ])
    .expect("lxc tools installed");
    LxcCli::new(
        Arc::new(RealExec::new(paths)),
        LXC_PATH.into(),
        vec![StoragePool::Dir {
            name: StorageName::new("test-dir").unwrap(),
            path: POOL.into(),
        }],
        PathBuf::from(POOL).join("tmp"),
    )
}

/// Destroys the container on drop, whatever state it is in.
struct Guard(ContainerName);

impl Drop for Guard {
    fn drop(&mut self) {
        let name = self.0.clone();
        // Drop can't be async; run cleanup on its own runtime in a plain thread.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let b = backend();
                let _ = b.kill(&name).await;
                let _ = b.destroy(&name).await;
            });
        })
        .join();
        let _ = std::fs::remove_dir_all(PathBuf::from(LXC_PATH).join(self.0.as_str()));
        let _ =
            std::fs::remove_dir_all(PathBuf::from(POOL).join("containers").join(self.0.as_str()));
    }
}

fn spec(name: &ContainerName) -> CreateSpec {
    CreateSpec {
        name: name.clone(),
        template: TemplateRef::default(),
        storage: StorageName::new("test-dir").unwrap(),
        limits: Limits {
            cores: Some(1),
            memory_mb: Some(512),
            ..Limits::default()
        },
        network: NetworkSpec {
            bridge: BridgeName::new("lxcbr0").unwrap(),
            mode: Default::default(),
        },
        gpu: false,
        nesting: false,
        autostart: false,
    }
}

#[tokio::test]
async fn create_start_inspect_stop_destroy() {
    let b = backend();
    let name = ContainerName::new("arilbox-test-create-01").unwrap();
    let _guard = Guard(name.clone());

    b.create(&spec(&name)).await.expect("create");
    let config =
        std::fs::read_to_string(PathBuf::from(LXC_PATH).join(name.as_str()).join("config"))
            .unwrap();
    assert!(config.starts_with("# Managed by arilboxd"), "{config}");
    assert!(!config.contains("lxc.include"), "{config}");

    let listed = b.list().await.unwrap();
    assert!(listed.iter().any(|c| c.name == name));
    assert_eq!(b.info(&name).await.unwrap().state, ContainerState::Stopped);

    b.start(&name).await.expect("start");
    let mut ip = None;
    for _ in 0..60 {
        let info = b.info(&name).await.unwrap();
        assert_eq!(info.state, ContainerState::Running);
        if let Some(addr) = info.ipv4.first() {
            ip = Some(*addr);
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(
        ip.is_some(),
        "no IPv4 address within 60 s (is lxc-net running?)"
    );

    // Unprivileged: container root is host UID 100000.
    let out = b
        .exec(&name, &["cat".into(), "/proc/self/uid_map".into()])
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    let map = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = map.split_whitespace().collect();
    assert_eq!(fields, ["0", "100000", "65536"], "{map}");

    // Limits applied from the generated config.
    let out = b
        .exec(&name, &["cat".into(), "/sys/fs/cgroup/memory.max".into()])
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "536870912");

    b.stop(&name, Duration::from_secs(30)).await.expect("stop");
    assert_eq!(b.info(&name).await.unwrap().state, ContainerState::Stopped);

    b.destroy(&name).await.expect("destroy");
    assert!(!PathBuf::from(LXC_PATH).join(name.as_str()).exists());
    assert!(
        !PathBuf::from(POOL)
            .join("containers")
            .join(name.as_str())
            .exists()
    );
}

#[tokio::test]
async fn foreign_containers_are_invisible() {
    let b = backend();
    // Whatever the developer has in /var/lib/lxc, only arilbox.json-marked ones show up.
    for c in b.list().await.unwrap() {
        assert!(
            PathBuf::from(LXC_PATH)
                .join(c.name.as_str())
                .join(arilxc::lxc_cli::META_FILE)
                .is_file()
        );
    }
}
