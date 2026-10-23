//! `GET /api/v1/storage` and `GET /api/v1/templates`: the choices for creating a container.

use arilbox_core::config::{StoragePool, TemplateRef};
use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::AppState;

/// A configured storage pool.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PoolInfo {
    name: String,
    #[serde(rename = "type")]
    kind: &'static str,
    /// Usable on this host (ZFS pools need the ZFS module).
    available: bool,
    /// Used when a create request names no pool.
    default: bool,
}

/// An allowed container template.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct TemplateInfo {
    #[serde(flatten)]
    template: TemplateRef,
    /// Used when a create request names no template.
    default: bool,
}

/// `GET /storage`: configured pools in config order.
pub(crate) async fn pools(State(state): State<AppState>) -> Json<Vec<PoolInfo>> {
    let settings = &state.settings;
    let zfs = state.host.capabilities().zfs;
    let default = settings
        .default_storage
        .as_ref()
        .or_else(|| settings.storage.first().map(StoragePool::name));
    Json(
        settings
            .storage
            .iter()
            .map(|pool| {
                let (kind, available) = match pool {
                    StoragePool::Zfs { .. } => ("zfs", zfs),
                    StoragePool::Dir { .. } => ("dir", true),
                };
                PoolInfo {
                    name: pool.name().to_string(),
                    kind,
                    available,
                    default: Some(pool.name()) == default,
                }
            })
            .collect(),
    )
}

/// `GET /templates`: the template allow-list (`SPEC.md` §5.5).
pub(crate) async fn templates(State(state): State<AppState>) -> Json<Vec<TemplateInfo>> {
    let default = &state.settings.default_template;
    Json(
        arilxc::template::allowed_templates()
            .map(|t| TemplateInfo {
                default: &t == default,
                template: t,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::routes::tests::{get, test_app};

    #[tokio::test]
    async fn lists_pools_with_availability() {
        let app = test_app(false).await;
        let (status, _, body) = get(app.router, "/api/v1/storage").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!([
                { "name": "local-dir", "type": "dir", "available": true, "default": true },
                { "name": "local-zfs", "type": "zfs", "available": false, "default": false }
            ])
        );
    }

    #[tokio::test]
    async fn lists_templates() {
        let app = test_app(false).await;
        let (status, _, body) = get(app.router, "/api/v1/templates").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!([
                { "distro": "debian", "release": "13", "default": true },
                { "distro": "ubuntu", "release": "24.04", "default": false },
                { "distro": "almalinux", "release": "10", "default": false },
                { "distro": "alpine", "release": "edge", "default": false }
            ])
        );
    }
}
