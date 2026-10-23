//! HTTP API for Arilbox (`SPEC.md` §7).
//!
//! [`router`] builds the axum router; the daemon serves it over HTTPS. Handlers only talk to
//! the host through the traits in [`AppState`]; slow changes run as [`jobs`]. The built web UI
//! is served from [`Settings::web_dir`] for every path outside `/api`.

pub mod auth;
pub mod db;
mod error;
mod extract;
pub mod jobs;
mod middleware;
mod routes;

use std::path::PathBuf;
use std::sync::Arc;

use arilbox_core::config::{StoragePool, TemplateRef};
use arilbox_core::host::HostProbe;
use arilbox_core::{BridgeName, StorageName};
use arilxc::ContainerBackend;
use axum::Router;
use axum::http::{HeaderName, HeaderValue, Request, header};
use axum::routing::any;
use sqlx::SqlitePool;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
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
    /// Built web UI (`index.html` and `assets/`). `None` serves only the API.
    pub web_dir: Option<PathBuf>,
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
    /// Database (jobs, audit log, users, sessions).
    pub db: SqlitePool,
    /// Users and sessions.
    pub auth: Arc<auth::Auth>,
    /// Validation settings.
    pub settings: Arc<Settings>,
}

/// `Content-Security-Policy` for the UI: everything from this origin, no inline scripts, no
/// framing. Inline styles are allowed because Svelte transitions and xterm.js set them.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data:; connect-src 'self'; font-src 'self'; object-src 'none'; \
                   base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Builds the full router: `/api/v1/...` plus request IDs, tracing, sessions, CSRF, audit,
/// security headers and (with a `web_dir`) the UI.
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

    let api = Router::new()
        .nest("/api/v1", routes::v1(&state))
        // Unknown API paths are JSON 404s even when the UI is served.
        .route("/api", any(routes::not_found))
        .route("/api/{*rest}", any(routes::not_found));
    let app = match &state.settings.web_dir {
        // Client-side routes (`/containers/web`) get index.html; the UI routes them.
        Some(dir) => api
            .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => api.fallback(routes::not_found),
    };

    app
        // Later layers wrap earlier ones: audit sees CSRF rejections too.
        .layer(axum::middleware::from_fn(middleware::csrf))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::audit,
        ))
        .with_state(state)
        .layer(axum::middleware::map_response(cache_control))
        .layer(header_layer(header::CONTENT_SECURITY_POLICY, CSP))
        .layer(header_layer(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(header_layer(header::X_FRAME_OPTIONS, "DENY"))
        .layer(header_layer(header::REFERRER_POLICY, "no-referrer"))
        // Outermost first: assign the ID, then trace with it, then echo it in the response.
        .layer(PropagateRequestIdLayer::new(request_id.clone()))
        .layer(trace)
        .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
}

fn header_layer(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
}

/// API responses are never cached; hashed UI assets forever; `index.html` is revalidated.
async fn cache_control(
    uri: axum::http::Uri,
    mut response: axum::response::Response,
) -> axum::response::Response {
    let path = uri.path();
    let value = if path == "/api" || path.starts_with("/api/") {
        "no-store"
    } else if path.starts_with("/assets/") && response.status().is_success() {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(value));
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::routes::tests::test_app;

    /// Temp directory removed on drop.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn serves_the_ui_with_client_side_routes() {
        let dir =
            TempDir(std::env::temp_dir().join(format!("arilbox-test-web-{}", std::process::id())));
        std::fs::create_dir_all(dir.0.join("assets")).unwrap();
        std::fs::write(dir.0.join("index.html"), "<!doctype html>ui").unwrap();
        std::fs::write(dir.0.join("assets/app-1234.js"), "js").unwrap();

        let app = test_app(false).await;
        let state = AppState {
            settings: Arc::new(Settings {
                web_dir: Some(dir.0.clone()),
                ..(*app.state.settings).clone()
            }),
            ..app.state.clone()
        };
        let router = router(state);
        let get = |uri: &'static str| {
            let router = router.clone();
            async move {
                let res = router
                    .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                let status = res.status();
                let cache = res.headers()["cache-control"].to_str().unwrap().to_owned();
                let csp = res.headers().contains_key("content-security-policy");
                let body = to_bytes(res.into_body(), 1 << 16).await.unwrap();
                (
                    status,
                    cache,
                    csp,
                    String::from_utf8_lossy(&body).into_owned(),
                )
            }
        };

        for uri in ["/", "/containers/web", "/login"] {
            let (status, cache, csp, body) = get(uri).await;
            assert_eq!(
                (status, cache.as_str(), csp, body.as_str()),
                (StatusCode::OK, "no-cache", true, "<!doctype html>ui"),
                "{uri}"
            );
        }
        let (status, cache, _, body) = get("/assets/app-1234.js").await;
        assert_eq!(
            (status, cache.as_str(), body.as_str()),
            (StatusCode::OK, "public, max-age=31536000, immutable", "js")
        );

        // API paths never fall through to the UI.
        for uri in ["/api", "/api/v1/nope", "/api/v2/host"] {
            let (status, cache, _, body) = get(uri).await;
            assert_eq!(
                (status, cache.as_str()),
                (StatusCode::NOT_FOUND, "no-store"),
                "{uri}"
            );
            assert!(body.contains("\"not_found\""), "{uri}: {body}");
        }
        // No escaping the web root.
        let (_, _, _, body) = get("/../Cargo.toml").await;
        assert!(!body.contains("[package]"));
    }
}
