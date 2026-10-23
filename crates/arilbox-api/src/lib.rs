//! HTTP API for Arilbox (`SPEC.md` §7).
//!
//! [`router`] builds the axum router; the daemon serves it over HTTPS. Handlers only talk to
//! the host through the traits in [`AppState`].

mod error;
mod routes;

use std::sync::Arc;

use arilbox_core::host::HostProbe;
use arilxc::ContainerBackend;
use axum::Router;
use axum::http::{HeaderName, Request};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

pub use error::ApiError;

/// Header carrying the per-request ID, generated if the client didn't send one.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Shared state for all handlers.
#[derive(Clone)]
pub struct AppState {
    /// Container backend (LXC in production, mock in tests).
    pub containers: Arc<dyn ContainerBackend>,
    /// Host information source.
    pub host: Arc<dyn HostProbe>,
}

/// Builds the full router: `/api/v1/...` plus request IDs and request tracing.
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
        .with_state(state)
        // Outermost first: assign the ID, then trace with it, then echo it in the response.
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(trace)
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}
