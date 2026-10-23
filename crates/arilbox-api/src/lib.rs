//! HTTP API for Arilbox (`SPEC.md` §7).
//!
//! [`router`] builds the axum router; the daemon serves it over HTTPS. Handlers only talk to
//! the host through the traits in [`AppState`]; slow changes run as [`jobs`].

pub mod db;
mod error;
mod extract;
pub mod jobs;
mod middleware;
mod routes;

use std::sync::Arc;

use arilbox_core::config::{StoragePool, TemplateRef};
use arilbox_core::host::HostProbe;
use arilbox_core::{BridgeName, StorageName};
use arilxc::ContainerBackend;
use axum::Router;
use axum::http::{HeaderName, Request};
use sqlx::SqlitePool;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

pub use error::ApiError;
pub use middleware::CSRF_HEADER;

/// Header carrying the per-request ID, generated if the client didn't send one.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Configuration the handlers need for validation.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Configured storage pools.
    pub storage: Vec<StoragePool>,
    /// Pool used when a request names none (else the first pool).
    pub default_storage: Option<StorageName>,
    /// Template used when a request names none.
    pub default_template: TemplateRef,
    /// The daemon-managed bridge.
    pub managed_bridge: BridgeName,
    /// Interface lookups (other bridges a request may name).
    pub sys: arilbox_net::Sys,
}

/// Shared state for all handlers.
#[derive(Clone)]
pub struct AppState {
    /// Container backend (LXC in production, mock in tests).
    pub containers: Arc<dyn ContainerBackend>,
    /// Host information source.
    pub host: Arc<dyn HostProbe>,
    /// Job runner.
    pub jobs: jobs::Jobs,
    /// Database (jobs, audit log).
    pub db: SqlitePool,
    /// Validation settings.
    pub settings: Arc<Settings>,
}

/// Builds the full router: `/api/v1/...` plus request IDs, tracing, CSRF and audit.
pub fn router(state: AppState) -> Router {
    let request_id = HeaderName::from_static(REQUEST_ID_HEADER);
    let trace = TraceLayer::new_for_http().make_span_with(|req: &Request<_>| {
        let id = req
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        tracing::info_span!("request", request_id = %id, method = %req.method(), path = %req.uri().path())
    });

    Router::new()
        .nest("/api/v1", routes::v1())
        .fallback(routes::not_found)
        // Later layers wrap earlier ones: audit sees CSRF rejections too.
        .layer(axum::middleware::from_fn(middleware::csrf))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::audit,
        ))
        .with_state(state)
        // Outermost first: assign the ID, then trace with it, then echo it in the response.
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(trace)
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}
