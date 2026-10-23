//! `/api/v1/host` endpoints (`SPEC.md` §7.2).

use arilbox_core::host::{Capabilities, HostInfo};
use axum::Json;
use axum::extract::State;

use crate::AppState;
use crate::error::ApiError;

/// `GET /api/v1/host`: hostname, OS, kernel, uptime, CPU/RAM totals, platform version.
pub(crate) async fn info(State(state): State<AppState>) -> Result<Json<HostInfo>, ApiError> {
    // Reads a handful of small procfs files; these never block on disk.
    Ok(Json(state.host.host_info()?))
}

/// `GET /api/v1/host/capabilities`: optional host features.
pub(crate) async fn capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    Json(state.host.capabilities())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::routes::tests::{app, get};

    #[tokio::test]
    async fn host_info_shape() {
        let (status, _, body) = get(app(false), "/api/v1/host").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({
                "hostname": "devbox",
                "os": "Debian GNU/Linux 13 (trixie)",
                "kernel": "6.12.48+deb13-amd64",
                "uptime_s": 42,
                "cpu_count": 8,
                "memory_total_bytes": 17_179_869_184_u64,
                "platform_version": "0.1.0"
            })
        );
    }

    #[tokio::test]
    async fn host_info_failure_is_opaque_500() {
        let (status, _, body) = get(app(true), "/api/v1/host").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "internal_error");
        assert!(
            !body.to_string().contains("meminfo"),
            "no internals: {body}"
        );
    }

    #[tokio::test]
    async fn capabilities_shape() {
        let (status, _, body) = get(app(false), "/api/v1/host/capabilities").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "zfs": true, "nvidia": false, "kvm": false, "cgroup_v2": true })
        );
    }
}
