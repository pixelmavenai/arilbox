//! Route table for `/api/v1`.
//!
//! M1: no authentication yet. Sessions and CSRF arrive in M3 (`SPEC.md` §12); until then the
//! daemon is for development only. `/health` stays public afterwards.

mod host;

use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

use crate::AppState;
use crate::error::ApiError;

/// Routes mounted under `/api/v1`.
pub(crate) fn v1() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/host", get(host::info))
        .route("/host/capabilities", get(host::capabilities))
}

/// `GET /api/v1/health`: liveness check.
async fn health() -> axum::Json<Value> {
    axum::Json(json!({ "status": "ok" }))
}

/// Fallback for unknown routes.
pub(crate) async fn not_found() -> ApiError {
    ApiError::not_found()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use arilbox_core::host::{Capabilities, HostInfo, HostProbe};
    use arilxc::MockBackend;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    use crate::{AppState, REQUEST_ID_HEADER, router};

    /// Fixed host data so tests don't depend on the machine.
    pub(crate) struct FakeProbe {
        pub fail: bool,
    }

    impl HostProbe for FakeProbe {
        fn host_info(&self) -> arilbox_core::Result<HostInfo> {
            if self.fail {
                return Err(arilbox_core::Error::Io {
                    context: "reading /proc/meminfo".into(),
                    source: std::io::Error::other("boom"),
                });
            }
            Ok(HostInfo {
                hostname: "devbox".into(),
                os: "Debian GNU/Linux 13 (trixie)".into(),
                kernel: "6.12.48+deb13-amd64".into(),
                uptime_s: 42,
                cpu_count: 8,
                memory_total_bytes: 16 * 1024 * 1024 * 1024,
                platform_version: "0.1.0".into(),
            })
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities {
                zfs: true,
                nvidia: false,
                kvm: false,
                cgroup_v2: true,
            }
        }
    }

    pub(crate) fn app(fail: bool) -> axum::Router {
        router(AppState {
            containers: Arc::new(MockBackend::new()),
            host: Arc::new(FakeProbe { fail }),
        })
    }

    /// Sends a GET and returns status, request-id header and JSON body.
    pub(crate) async fn get(app: axum::Router, uri: &str) -> (StatusCode, Option<String>, Value) {
        let res = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let id = res
            .headers()
            .get(REQUEST_ID_HEADER)
            .map(|v| v.to_str().unwrap().to_owned());
        let body = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, id, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn health_ok() {
        let (status, _, body) = get(app(false), "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn unknown_route_is_json_404() {
        for uri in ["/api/v1/nope", "/nope", "/api/v2/host"] {
            let (status, _, body) = get(app(false), uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body["error"]["code"], "not_found", "{uri}");
        }
    }

    #[tokio::test]
    async fn every_response_has_a_request_id() {
        let (_, id, _) = get(app(false), "/api/v1/health").await;
        let id = id.expect("x-request-id header");
        assert_eq!(id.len(), 36, "uuid: {id}");
    }

    #[tokio::test]
    async fn client_request_id_is_propagated() {
        let res = app(false)
            .oneshot(
                Request::get("/api/v1/health")
                    .header(REQUEST_ID_HEADER, "abc-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.headers()[REQUEST_ID_HEADER], "abc-123");
    }
}
