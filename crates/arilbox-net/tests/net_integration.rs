//! Real bridge + nftables test. Run as root:
//!
//! ```text
//! sudo -E cargo test -p arilbox-net --features lxc-integration -- --test-threads=1
//! ```
//!
//! Creates only the bridge `arilbox-test-0` and the nftables table `arilbox_test`, and removes
//! both afterwards (also on failure). Leaves `net.ipv4.ip_forward = 1`.
#![cfg(feature = "lxc-integration")]
// Test code: panicking on unexpected errors is the point.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arilbox_core::BridgeName;
use arilbox_core::config::Ipv4Cidr;
use arilbox_core::exec::{CommandSpec, Exec, Program, ProgramPaths, RealExec};
use arilbox_net::{ManagedNetwork, Sys, bridge_del_cmd, nft_delete_cmd, setup};

const BRIDGE: &str = "arilbox-test-0";
const TABLE: &str = "arilbox_test";

fn exec() -> RealExec {
    RealExec::new(ProgramPaths::resolve(&[Program::Ip, Program::Nft]).expect("ip and nft"))
}

struct Guard;

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = std::thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let e = exec();
                let _ = e
                    .run(bridge_del_cmd(&BridgeName::new(BRIDGE).unwrap()))
                    .await;
                let _ = e.run(nft_delete_cmd(TABLE)).await;
            });
        })
        .join();
    }
}

#[tokio::test]
async fn setup_is_idempotent_and_correct() {
    let _guard = Guard;
    let e = exec();
    let state = std::env::temp_dir().join("arilbox-test-net-state");
    let net = ManagedNetwork {
        bridge: BridgeName::new(BRIDGE).unwrap(),
        subnet: Ipv4Cidr::try_from("10.250.77.1/24".to_owned()).unwrap(),
        nft_table: TABLE,
        state_dir: state.clone(),
    };

    setup(&e, &Sys::default(), &net).await.expect("first setup");
    setup(&e, &Sys::default(), &net)
        .await
        .expect("second setup is a no-op");

    assert!(Sys::default().is_bridge(&net.bridge));
    let addr = e
        .run(CommandSpec::new(
            Program::Ip,
            ["-4", "-o", "addr", "show", "dev", BRIDGE],
        ))
        .await
        .unwrap();
    let addr = String::from_utf8_lossy(&addr.stdout);
    assert!(addr.contains("10.250.77.1/24"), "{addr}");
    assert_eq!(
        addr.matches("inet ").count(),
        1,
        "address not duplicated: {addr}"
    );

    let rules = e
        .run(CommandSpec::new(
            Program::Nft,
            ["list", "table", "ip", TABLE],
        ))
        .await
        .unwrap();
    let rules = String::from_utf8_lossy(&rules.stdout);
    assert!(
        rules.contains("10.250.77.0/24") && rules.contains("masquerade"),
        "{rules}"
    );
    assert_eq!(
        rules.matches("masquerade").count(),
        1,
        "rule not duplicated: {rules}"
    );

    assert_eq!(
        std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
            .unwrap()
            .trim(),
        "1"
    );
    let _ = std::fs::remove_dir_all(state);
}
