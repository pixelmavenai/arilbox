//! CSRF check and audit log (`SPEC.md` §12).

use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use time::OffsetDateTime;

use crate::error::ApiError;
use crate::{AppState, REQUEST_ID_HEADER};

/// Header every mutating request must carry, with value `1`.
pub const CSRF_HEADER: &str = "x-arilbox-request";

fn is_mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Rejects mutating requests without `X-Arilbox-Request: 1` (`403 csrf_failed`).
pub async fn csrf(req: Request, next: Next) -> Response {
    if is_mutating(req.method())
        && req.headers().get(CSRF_HEADER).map(|v| v.as_bytes()) != Some(b"1")
    {
        return ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "csrf_failed",
            "Missing X-Arilbox-Request header",
        )
        .into_response();
    }
    next.run(req).await
}

/// Set by a handler on its response when the audit target is not in the path (e.g. create).
#[derive(Debug, Clone)]
pub struct AuditTarget(pub String);

/// Writes an audit entry for every mutating request, including rejected ones.
pub async fn audit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !is_mutating(req.method()) {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let template = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let response = next.run(req).await;

    let action = action_name(&method, template.as_deref(), &path);
    let target = response
        .extensions()
        .get::<AuditTarget>()
        .map(|t| t.0.clone())
        .or_else(|| template.as_deref().and_then(|t| name_param(t, &path)));
    let result = sqlx::query(
        "INSERT INTO audit_log (at, username, action, target, status, request_id) \
         VALUES (?, NULL, ?, ?, ?, ?)",
    )
    .bind(
        OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    )
    .bind(&action)
    .bind(&target)
    .bind(i64::from(response.status().as_u16()))
    .bind(&request_id)
    .execute(&state.db)
    .await;
    if let Err(e) = result {
        tracing::error!(error = %e, %action, "writing audit log");
    }
    response
}

/// Stable action names for known routes; `METHOD /path/template` otherwise.
fn action_name(method: &Method, template: Option<&str>, path: &str) -> String {
    let known = match (method.as_str(), template) {
        ("POST", Some("/api/v1/containers")) => Some("container.create"),
        ("DELETE", Some("/api/v1/containers/{name}")) => Some("container.delete"),
        ("POST", Some("/api/v1/containers/{name}/start")) => Some("container.start"),
        ("POST", Some("/api/v1/containers/{name}/stop")) => Some("container.stop"),
        ("POST", Some("/api/v1/containers/{name}/restart")) => Some("container.restart"),
        _ => None,
    };
    known.map_or_else(
        || format!("{method} {}", template.unwrap_or(path)),
        str::to_owned,
    )
}

/// The path segment matching `{name}` in the route template.
fn name_param(template: &str, path: &str) -> Option<String> {
    template
        .split('/')
        .zip(path.split('/'))
        .find(|(t, _)| *t == "{name}")
        .map(|(_, v)| v.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            action_name(&Method::POST, Some("/api/v1/containers/{name}/start"), "/x"),
            "container.start"
        );
        assert_eq!(
            action_name(&Method::POST, None, "/api/v1/nope"),
            "POST /api/v1/nope"
        );
        assert_eq!(
            name_param(
                "/api/v1/containers/{name}/stop",
                "/api/v1/containers/web/stop"
            ),
            Some("web".into())
        );
        assert_eq!(name_param("/api/v1/containers", "/api/v1/containers"), None);
    }
}
