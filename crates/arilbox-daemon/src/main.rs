//! `arilboxd`: the Arilbox daemon.

mod args;
mod server;
mod tls;

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use arilbox_api::AppState;
use arilbox_core::config::Config;
use arilbox_core::host::{HostProbe, SystemProbe};
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

    // M1: in-memory backend only. Replaced by `LxcCli` in M2.
    tracing::warn!("using in-memory mock container backend (M1 skeleton)");
    let state = AppState {
        containers: Arc::new(arilxc::MockBackend::new()),
        host: Arc::new(probe),
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
