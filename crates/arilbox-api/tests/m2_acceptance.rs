//! M2 acceptance (`SPEC.md` §19): create a Debian 13 container through the job runner; it gets
//! an IP from Arilbox's dnsmasq and reaches the internet through Arilbox's NAT. Run as root
//! on a host with internet access:
//!
//! ```text
//! sudo -E cargo test -p arilbox-api --features lxc-integration -- --test-threads=1 --nocapture
//! ```
//!
//! Uses its own bridge `arilbox-test-0` (10.250.77.1/24), nftables table `arilbox_test`, dir
//! pool `/var/lib/arilbox/test` and containers `arilbox-test-*`. A drop guard removes all of
//! them, also on failure.
#![cfg(feature = "lxc-integration")]
// Test code: panicking on unexpected errors is the point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arilbox_api::jobs::container::{create_job, delete_job, stop_job};
use arilbox_api::jobs::{JobRecord, JobStatus, Jobs};
use arilbox_core::config::{Ipv4Cidr, StoragePool, TemplateRef};
use arilbox_core::exec::{Exec, Program, ProgramPaths, RealExec};
use arilbox_core::{BridgeName, ContainerName, StorageName};
use arilbox_net::{ManagedNetwork, Sys, bridge_del_cmd, nft_delete_cmd};
use arilxc::{ContainerBackend, CreateSpec, Limits, LxcCli, NetworkSpec};
use uuid::Uuid;

const LXC_PATH: &str = "/var/lib/lxc";
const POOL: &str = "/var/lib/arilbox/test";
const BRIDGE: &str = "arilbox-test-0";
const TABLE: &str = "arilbox_test";

fn exec() -> Arc<RealExec> {
    Arc::new(RealExec::new(
        ProgramPaths::resolve(&[
            Program::LxcCreate,
            Program::LxcStart,
            Program::LxcStop,
            Program::LxcInfo,
            Program::LxcAttach,
            Program::LxcDestroy,
            Program::Ip,
            Program::Nft,
            Program::Dnsmasq,
        ])
        .expect("required programs installed"),
    ))
}

fn backend(exec: Arc<RealExec>) -> LxcCli {
    LxcCli::new(
        exec,
        LXC_PATH.into(),
        vec![StoragePool::Dir {
            name: StorageName::new("test-dir").unwrap(),
            path: POOL.into(),
        }],
        PathBuf::from(POOL).join("tmp"),
    )
}

/// Removes containers, bridge and nft table on drop.
struct Guard(Vec<ContainerName>);

impl Drop for Guard {
    fn drop(&mut self) {
        let names = self.0.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let e = exec();
                let b = backend(e.clone());
                for name in &names {
                    let _ = b.kill(name).await;
                    let _ = b.destroy(name).await;
                }
                let _ = e
                    .run(bridge_del_cmd(&BridgeName::new(BRIDGE).unwrap()))
                    .await;
                let _ = e.run(nft_delete_cmd(TABLE)).await;
            });
        })
        .join();
    }
}

async fn wait(jobs: &Jobs, id: Uuid) -> JobRecord {
    for _ in 0..1200 {
        let job = jobs.get(id).await.unwrap().unwrap();
        if matches!(job.status, JobStatus::Succeeded | JobStatus::Failed)
            && !jobs.is_busy(&job.target)
        {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("job {id} did not finish in 10 minutes");
}

fn spec(name: &ContainerName, bridge: &str) -> CreateSpec {
    CreateSpec {
        name: name.clone(),
        template: TemplateRef::default(),
        storage: StorageName::new("test-dir").unwrap(),
        limits: Limits {
            cores: Some(1),
            memory_mb: Some(1024),
            ..Limits::default()
        },
        network: NetworkSpec {
            bridge: BridgeName::new(bridge).unwrap(),
            mode: Default::default(),
        },
        gpu: false,
        nesting: false,
        autostart: false,
    }
}

#[tokio::test]
async fn debian_container_gets_ip_and_reaches_internet() {
    let ok = ContainerName::new("arilbox-test-accept-01").unwrap();
    let bad = ContainerName::new("arilbox-test-rollback-01").unwrap();
    let _guard = Guard(vec![ok.clone(), bad.clone()]);

    let e = exec();
    let state_dir = std::env::temp_dir().join("arilbox-test-accept-net");
    let net = ManagedNetwork {
        bridge: BridgeName::new(BRIDGE).unwrap(),
        subnet: Ipv4Cidr::try_from("10.250.77.1/24".to_owned()).unwrap(),
        nft_table: TABLE,
        state_dir: state_dir.clone(),
    };
    arilbox_net::setup(e.as_ref(), &Sys::default(), &net)
        .await
        .expect("network setup");
    let dnsmasq = arilbox_net::supervise_dnsmasq(e.clone(), &net).unwrap();

    let containers: Arc<dyn ContainerBackend> = Arc::new(backend(e.clone()));
    let db = arilbox_api::db::memory().await.unwrap();
    let jobs = Jobs::new(db, containers.clone());

    // 1. Create + start + wait for network, all through the job runner.
    let id = jobs
        .submit(create_job(spec(&ok, BRIDGE), true, Duration::from_secs(90)))
        .await
        .unwrap();
    let job = wait(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:#?}");

    let info = containers.info(&ok).await.unwrap();
    let ip = info.ipv4.first().copied().expect("an IPv4 address");
    println!("container address: {ip}");
    assert_eq!(
        &ip.octets()[..3],
        &[10, 250, 77],
        "address from our dnsmasq"
    );

    // 2. DNS through dnsmasq, then HTTP to the Debian mirror through NAT.
    let dns = containers
        .exec(
            &ok,
            &["getent".into(), "hosts".into(), "deb.debian.org".into()],
        )
        .await
        .unwrap();
    assert_eq!(
        dns.exit_code,
        0,
        "DNS failed: {}",
        String::from_utf8_lossy(&dns.stderr)
    );
    let apt = containers
        .exec(&ok, &["apt-get".into(), "update".into(), "-q".into()])
        .await
        .unwrap();
    assert_eq!(
        apt.exit_code,
        0,
        "apt-get update failed: {}",
        String::from_utf8_lossy(&apt.stderr)
    );

    // 3. Stop and delete through jobs; nothing left.
    let job = wait(
        &jobs,
        jobs.submit(stop_job(ok.clone(), Duration::from_secs(30)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:#?}");
    let job = wait(
        &jobs,
        jobs.submit(delete_job(ok.clone(), false)).await.unwrap(),
    )
    .await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:#?}");
    assert!(!PathBuf::from(LXC_PATH).join(ok.as_str()).exists());

    // 4. Forced failure: a bridge that doesn't exist makes start fail; the create is rolled
    //    back and leaves nothing behind.
    let id = jobs
        .submit(create_job(
            spec(&bad, "arilbox-test-x"),
            true,
            Duration::from_secs(30),
        ))
        .await
        .unwrap();
    let job = wait(&jobs, id).await;
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(
        job.error.as_ref().unwrap().step.as_deref(),
        Some("start_container")
    );
    assert!(!job.error.as_ref().unwrap().rollback_incomplete, "{job:#?}");
    assert!(!PathBuf::from(LXC_PATH).join(bad.as_str()).exists());
    assert!(
        !PathBuf::from(POOL)
            .join("containers")
            .join(bad.as_str())
            .exists()
    );

    // Wait until dnsmasq is really gone before the guard deletes the bridge; otherwise it
    // logs "error binding DHCP socket" when the interface disappears under it.
    dnsmasq.abort();
    let _ = dnsmasq.await;
    let _ = std::fs::remove_dir_all(state_dir);
}
