//! Route table for `/api/v1`.
//!
//! No authentication until M3 (sessions, `SPEC.md` §12); until then the daemon is for
//! development only. The CSRF header and the audit log already apply. `/health` stays public.

mod containers;
mod host;
mod jobs;

use axum::Router;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::AppState;
use crate::error::ApiError;

/// Routes mounted under `/api/v1`.
pub(crate) fn v1() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/host", get(host::info))
        .route("/host/capabilities", get(host::capabilities))
        .route(
            "/containers",
            get(containers::list).post(containers::create),
        )
        .route(
            "/containers/{name}",
            get(containers::get).delete(containers::delete),
        )
        .route("/containers/{name}/start", post(containers::start))
        .route("/containers/{name}/stop", post(containers::stop))
        .route("/containers/{name}/restart", post(containers::restart))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::get))
        .route("/jobs/{id}/events", get(jobs::events))
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

    use arilbox_core::config::{StoragePool, TemplateRef};
    use arilbox_core::{BridgeName, DatasetPath, StorageName};

    use crate::{AppState, REQUEST_ID_HEADER, Settings, router};

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
                zfs: false,
                nvidia: false,
                kvm: false,
                cgroup_v2: true,
            }
        }
    }

    /// Router plus handles on its state, backed by the mock backend and in-memory SQLite.
    pub(crate) struct TestApp {
        pub router: axum::Router,
        pub state: AppState,
        pub backend: Arc<MockBackend>,
    }

    pub(crate) async fn test_app(fail: bool) -> TestApp {
        let backend = Arc::new(MockBackend::new());
        let db = crate::db::memory().await.unwrap();
        let mut jobs = crate::jobs::Jobs::new(db.clone(), backend.clone());
        jobs.network_timeout = std::time::Duration::from_millis(200);
        let state = AppState {
            containers: backend.clone(),
            host: Arc::new(FakeProbe { fail }),
            jobs,
            db,
            settings: Arc::new(Settings {
                storage: vec![
                    StoragePool::Dir {
                        name: StorageName::new("local-dir").unwrap(),
                        path: "/var/lib/arilbox/storage".into(),
                    },
                    StoragePool::Zfs {
                        name: StorageName::new("local-zfs").unwrap(),
                        dataset: DatasetPath::new("tank/arilbox").unwrap(),
                    },
                ],
                default_storage: Some(StorageName::new("local-dir").unwrap()),
                default_template: TemplateRef::default(),
                managed_bridge: BridgeName::new("arilbox0").unwrap(),
                sys: arilbox_net::Sys::new("/nonexistent-arilbox-test-sys"),
            }),
        };
        TestApp {
            router: router(state.clone()),
            state,
            backend,
        }
    }

    pub(crate) async fn app(fail: bool) -> axum::Router {
        test_app(fail).await.router
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
        let (status, _, body) = get(app(false).await, "/api/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn unknown_route_is_json_404() {
        for uri in ["/api/v1/nope", "/nope", "/api/v2/host"] {
            let (status, _, body) = get(app(false).await, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(body["error"]["code"], "not_found", "{uri}");
        }
    }

    #[tokio::test]
    async fn every_response_has_a_request_id() {
        let (_, id, _) = get(app(false).await, "/api/v1/health").await;
        let id = id.expect("x-request-id header");
        assert_eq!(id.len(), 36, "uuid: {id}");
    }

    #[tokio::test]
    async fn client_request_id_is_propagated() {
        let res = app(false)
            .await
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
