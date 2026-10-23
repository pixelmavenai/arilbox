//! `arilboxd`: the Arilbox daemon.

mod args;
mod server;
mod tls;

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use arilbox_api::auth::{Auth, AuthConfig};
use arilbox_api::jobs::Jobs;
use arilbox_api::{AppState, Settings};
use arilbox_core::config::{BackendKind, Config, StoragePool};
use arilbox_core::exec::{Program, ProgramPaths, RealExec};
use arilbox_core::host::{HostProbe, SystemProbe};
use arilbox_net::{ManagedNetwork, Sys};
use arilxc::ContainerBackend;
use tracing_subscriber::EnvFilter;

use crate::args::Args;

fn main() -> anyhow::Result<()> {
    let args = match Args::parse(std::env::args_os().skip(1))? {
        args::Parsed::Run(args) => args,
        args::Parsed::Exit(text) => {
            println!("{text}");
            return Ok(());
        }
    };
    let config = Config::load(&args.config)
        .with_context(|| format!("loading config {}", args.config.display()))?;
    init_tracing(config.server.dev_mode);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting tokio runtime")?
        .block_on(run(config))
}

/// Programs the lxc backend and network setup need; checked at start-up.
const REQUIRED_PROGRAMS: &[Program] = &[
    Program::LxcCreate,
    Program::LxcStart,
    Program::LxcStop,
    Program::LxcInfo,
    Program::LxcAttach,
    Program::LxcDestroy,
    Program::Ip,
    Program::Nft,
    Program::Dnsmasq,
];

/// Effective UID 0, read from `/proc/self/status` (no libc needed).
fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|ids| ids.split_whitespace().nth(1).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

fn init_tracing(dev_mode: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if dev_mode {
        builder.init();
    } else {
        // journald stores stdout; JSON keeps fields machine-readable.
        builder.json().flatten_event(true).init();
    }
}

async fn run(config: Config) -> anyhow::Result<()> {
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "arilboxd starting");

    let probe = SystemProbe::default();
    let caps = probe.capabilities();
    tracing::info!(
        zfs = caps.zfs,
        nvidia = caps.nvidia,
        kvm = caps.kvm,
        cgroup_v2 = caps.cgroup_v2,
        "host capabilities"
    );
    if !caps.cgroup_v2 {
        tracing::warn!("cgroup v2 unified hierarchy not found; container limits will not work");
    }

    create_private_dir(&config.paths.data_dir)?;
    for pool in &config.storage {
        if matches!(pool, StoragePool::Zfs { .. }) && !caps.zfs {
            tracing::warn!(pool = %pool.name(), "ZFS is not available; this pool cannot be used");
        }
    }

    let db = arilbox_api::db::open(&config.paths.data_dir.join("arilbox.db"))
        .await
        .context("opening database")?;

    // Kept alive until shutdown; dropping it would kill dnsmasq.
    let mut _dnsmasq = None;
    let containers: Arc<dyn ContainerBackend> = match config.containers.backend {
        BackendKind::Mock => {
            tracing::warn!("using the in-memory mock container backend (dev mode)");
            Arc::new(arilxc::MockBackend::new())
        }
        BackendKind::Lxc => {
            if !is_root() {
                anyhow::bail!("arilboxd must run as root with the lxc backend");
            }
            let exec = Arc::new(RealExec::new(
                ProgramPaths::resolve(REQUIRED_PROGRAMS).context("required program missing")?,
            ));
            let net = ManagedNetwork {
                bridge: config.network.bridge.clone(),
                subnet: config.network.subnet,
                nft_table: "arilbox",
                state_dir: config.paths.data_dir.join("net"),
            };
            arilbox_net::setup(exec.as_ref(), &Sys::default(), &net)
                .await
                .context("setting up the container network")?;
            _dnsmasq = Some(
                arilbox_net::supervise_dnsmasq(exec.clone(), &net)
                    .context("subnet has no room for DHCP addresses")?,
            );
            Arc::new(arilxc::LxcCli::new(
                exec,
                config.containers.lxc_path.clone(),
                config.storage.clone(),
                config.paths.data_dir.join("tmp"),
            ))
        }
    };

    let jobs = Jobs::new(db.clone(), containers.clone());
    let interrupted = jobs.mark_interrupted().await.context("recovering jobs")?;
    if interrupted > 0 {
        tracing::warn!(
            count = interrupted,
            "marked jobs from a previous run as failed"
        );
    }

    let auth = Arc::new(
        Auth::new(
            db.clone(),
            AuthConfig {
                secure_cookie: config.server.tls,
                ..AuthConfig::default()
            },
        )
        .context("password hashing parameters")?,
    );
    if let Some(token) = auth.start_setup().await.context("checking for users")? {
        let scheme = if config.server.tls { "https" } else { "http" };
        // The one place a secret is logged on purpose (SPEC.md §12): it is single-use, only
        // valid until an admin exists, and replaced on every restart.
        tracing::warn!(
            "first-run setup: no admin user yet. Open {scheme}://<this host>:{port}/setup?token={token}",
            port = config.server.bind.port(),
        );
    }

    let web_dir = config
        .paths
        .web_dir
        .join("index.html")
        .is_file()
        .then(|| config.paths.web_dir.clone());
    if web_dir.is_none() {
        tracing::warn!(dir = %config.paths.web_dir.display(), "web UI not found; serving the API only");
    }

    let state = AppState {
        containers,
        host: Arc::new(probe),
        jobs,
        db,
        auth,
        settings: Arc::new(Settings {
            storage: config.storage.clone(),
            default_storage: config.defaults.storage.clone(),
            default_template: config.defaults.template.clone(),
            managed_bridge: config.network.bridge.clone(),
            sys: Sys::default(),
            web_dir,
        }),
    };
    let app = arilbox_api::router(state);

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;

    if config.server.tls {
        let acceptor = tls::acceptor(&config.server.tls_cert, &config.server.tls_key)?;
        tracing::info!(bind = %config.server.bind, "listening (https)");
        server::serve_tls(listener, acceptor, app, shutdown_signal()).await
    } else {
        // Config validation only allows this with dev_mode = true.
        tracing::warn!(bind = %config.server.bind, "listening on PLAIN HTTP (dev mode)");
        let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("http server")
    }
}

/// Creates `dir` (and parents) with mode 0700 if missing.
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

/// Resolves on SIGINT or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("shutting down");
}
