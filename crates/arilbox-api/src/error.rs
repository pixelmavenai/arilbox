//! Mapping of crate errors to the standard error body (`SPEC.md` §7).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// An error response: `{ "error": { "code", "message" } }`.
///
/// `code` is a stable `snake_case` identifier the UI translates; never rename one. `message`
/// is English and never contains stderr, paths or internal details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status.
    pub status: StatusCode,
    /// Stable error code.
    pub code: &'static str,
    /// Human-readable English message.
    pub message: String,
}

impl ApiError {
    /// Builds an error.
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// `404 not_found` for unknown routes.
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found")
    }

    /// `500 internal_error`. Log the cause before returning this.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Internal error; see the server log",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        (self.status, Json(body)).into_response()
    }
}

impl From<arilbox_core::Error> for ApiError {
    fn from(err: arilbox_core::Error) -> Self {
        match err {
            arilbox_core::Error::InvalidName { kind, reason } => {
                let code = match kind {
                    "container name" | "snapshot name" | "storage name" | "bridge name"
                    | "dataset path" => "invalid_name",
                    _ => "invalid_request",
                };
                Self::new(
                    StatusCode::BAD_REQUEST,
                    code,
                    format!("Invalid {kind}: {reason}"),
                )
            }
            other => {
                tracing::error!(error = %other, "internal error");
                Self::internal()
            }
        }
    }
}

impl From<arilxc::Error> for ApiError {
    fn from(err: arilxc::Error) -> Self {
        use arilxc::Error as E;
        match err {
            E::NotFound(name) => Self::new(
                StatusCode::NOT_FOUND,
                "container_not_found",
                format!("Container '{name}' does not exist"),
            ),
            E::Exists(name) => Self::new(
                StatusCode::CONFLICT,
                "container_exists",
                format!("Container '{name}' already exists"),
            ),
            E::WrongState { name, state } => {
                let code = match state {
                    arilxc::ContainerState::Running => "container_running",
                    arilxc::ContainerState::Stopped => "container_stopped",
                    _ => "container_wrong_state",
                };
                Self::new(
                    StatusCode::CONFLICT,
                    code,
                    format!("Container '{name}' is {state}"),
                )
            }
            E::SnapshotNotFound { name, snap } => Self::new(
                StatusCode::NOT_FOUND,
                "snapshot_not_found",
                format!("Snapshot '{snap}' of container '{name}' does not exist"),
            ),
            E::SnapshotExists { name, snap } => Self::new(
                StatusCode::CONFLICT,
                "snapshot_exists",
                format!("Snapshot '{snap}' of container '{name}' already exists"),
            ),
            E::Unsupported(what) => Self::new(
                StatusCode::BAD_REQUEST,
                "unsupported",
                format!("Not supported: {what}"),
            ),
            E::Backend(detail) => {
                tracing::error!(%detail, "container backend error");
                Self::internal()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arilbox_core::{ContainerName, SnapshotName};

    fn n() -> ContainerName {
        ContainerName::new("web").unwrap()
    }

    #[test]
    fn container_errors_map_to_spec_codes() {
        let cases: Vec<(arilxc::Error, StatusCode, &str)> = vec![
            (
                arilxc::Error::NotFound(n()),
                StatusCode::NOT_FOUND,
                "container_not_found",
            ),
            (
                arilxc::Error::Exists(n()),
                StatusCode::CONFLICT,
                "container_exists",
            ),
            (
                arilxc::Error::WrongState {
                    name: n(),
                    state: arilxc::ContainerState::Running,
                },
                StatusCode::CONFLICT,
                "container_running",
            ),
            (
                arilxc::Error::SnapshotNotFound {
                    name: n(),
                    snap: SnapshotName::new("s").unwrap(),
                },
                StatusCode::NOT_FOUND,
                "snapshot_not_found",
            ),
            (
                arilxc::Error::Backend("lxc-start: secret stderr".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            ),
        ];
        for (err, status, code) in cases {
            let api = ApiError::from(err);
            assert_eq!((api.status, api.code), (status, code));
        }
    }

    #[test]
    fn spec_example_message() {
        let api = ApiError::from(arilxc::Error::NotFound(n()));
        assert_eq!(api.message, "Container 'web' does not exist");
    }

    #[test]
    fn backend_details_are_not_exposed() {
        let api = ApiError::from(arilxc::Error::Backend("/var/lib/lxc stderr".into()));
        assert!(!api.message.contains("stderr"));
    }

    #[test]
    fn validation_is_400() {
        let api = ApiError::from(ContainerName::new("-x").unwrap_err());
        assert_eq!(
            (api.status, api.code),
            (StatusCode::BAD_REQUEST, "invalid_name")
        );
    }
}
