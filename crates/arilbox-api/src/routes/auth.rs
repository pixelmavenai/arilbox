//! `/api/v1/auth` (`SPEC.md` §7.1, §12).
//!
//! `GET`/`POST /auth/setup` and `POST /auth/login` are public; the rest need a session.

use arilbox_core::Username;
use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::auth::{CurrentUser, NewSession};
use crate::error::ApiError;
use crate::extract::{ApiJson, ClientIp};
use crate::middleware::{AuditTarget, AuditUser};

/// `POST /auth/setup` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetupRequest {
    token: String,
    username: String,
    password: String,
}

/// `POST /auth/login` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRequest {
    username: String,
    password: String,
}

/// `POST /auth/password` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordRequest {
    current: String,
    new: String,
}

/// Response for a new session: `{ "username" }` plus the cookie.
fn session_response(state: &AppState, status: StatusCode, session: &NewSession) -> Response {
    let mut response = (
        status,
        [(
            header::SET_COOKIE,
            state.auth.session_cookie(&session.token),
        )],
        Json(json!({ "username": session.user.username })),
    )
        .into_response();
    response
        .extensions_mut()
        .insert(AuditUser::from(&session.user));
    response
}

/// Records the attempted username (if well-formed) as the audit target, also on failure.
fn with_target(mut response: Response, username: Option<&Username>) -> Response {
    if let Some(name) = username {
        response
            .extensions_mut()
            .insert(AuditTarget(name.to_string()));
    }
    response
}

/// `GET /auth/setup`: `{ "required": bool }`.
pub(crate) async fn setup_status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        json!({ "required": state.auth.setup_required().await? }),
    ))
}

/// `POST /auth/setup`: creates the admin user with the one-time token → 201 and a session.
pub(crate) async fn setup(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<SetupRequest>,
) -> Response {
    let username = match Username::new(req.username) {
        Ok(name) => name,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let result = state
        .auth
        .complete_setup(ip, &req.token, username.clone(), &req.password)
        .await
        .map(|s| session_response(&state, StatusCode::CREATED, &s));
    with_target(
        result.unwrap_or_else(IntoResponse::into_response),
        Some(&username),
    )
}

/// `POST /auth/login` → `{ "username" }` and a session cookie.
pub(crate) async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(req): ApiJson<LoginRequest>,
) -> Response {
    let target = Username::new(req.username.as_str()).ok();
    let result = state
        .auth
        .login(ip, &req.username, &req.password)
        .await
        .map(|s| session_response(&state, StatusCode::OK, &s));
    with_target(
        result.unwrap_or_else(IntoResponse::into_response),
        target.as_ref(),
    )
}

/// `POST /auth/logout` → 204, cookie cleared.
pub(crate) async fn logout(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    state.auth.logout(&user).await?;
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, state.auth.clear_cookie())],
    )
        .into_response())
}

/// `GET /auth/me` → `{ "username" }`.
pub(crate) async fn me(user: CurrentUser) -> Json<serde_json::Value> {
    Json(json!({ "username": user.username }))
}

/// `POST /auth/password` → 204. Other sessions of the user end.
pub(crate) async fn change_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    user: CurrentUser,
    ApiJson(req): ApiJson<PasswordRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .auth
        .change_password(ip, &user, &req.current, &req.new)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::CSRF_HEADER;
    use crate::routes::tests::{PASSWORD, TestApp, fresh_app, test_app};

    struct Res {
        status: StatusCode,
        headers: axum::http::HeaderMap,
        body: Value,
    }

    async fn call(
        router: &axum::Router,
        method: Method,
        uri: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> Res {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1");
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let res = router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        Res {
            status,
            headers,
            body,
        }
    }

    fn set_cookie(res: &Res) -> String {
        res.headers["set-cookie"].to_str().unwrap().to_owned()
    }

    /// `name=value` from a `Set-Cookie` header.
    fn cookie_pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_owned()
    }

    async fn audit(app: &TestApp) -> Vec<(Option<String>, String, Option<String>, i64)> {
        sqlx::query_as("SELECT username, action, target, status FROM audit_log ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn first_run_setup_over_http() {
        let app = fresh_app(false).await;
        let r = call(&app.anon, Method::GET, "/api/v1/auth/setup", None, None).await;
        assert_eq!(
            (r.status, r.body),
            (StatusCode::OK, json!({ "required": true }))
        );

        let token = app.state.auth.start_setup().await.unwrap().unwrap();
        let body = |token: &str, username: &str| json!({ "token": token, "username": username, "password": PASSWORD });
        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/setup",
            Some(body("bad", "admin")),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("invalid_setup_token"))
        );
        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/setup",
            Some(body(&token, "Admin!")),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_username"))
        );
        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/setup",
            Some(
                json!({ "token": token, "username": "admin", "password": PASSWORD, "admin": true }),
            ),
            None,
        )
        .await;
        assert_eq!(r.body["error"]["code"], "invalid_request", "unknown field");

        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/setup",
            Some(body(&token, "admin")),
            None,
        )
        .await;
        assert_eq!(
            (r.status, &r.body),
            (StatusCode::CREATED, &json!({ "username": "admin" }))
        );
        let cookie = cookie_pair(&set_cookie(&r));
        assert!(set_cookie(&r).contains("HttpOnly; SameSite=Strict"));

        let r = call(
            &app.anon,
            Method::GET,
            "/api/v1/auth/me",
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(
            (r.status, r.body),
            (StatusCode::OK, json!({ "username": "admin" }))
        );
        let r = call(&app.anon, Method::GET, "/api/v1/auth/setup", None, None).await;
        assert_eq!(r.body, json!({ "required": false }));

        let rows = audit(&app).await;
        assert_eq!(
            rows.last().unwrap(),
            &(
                Some("admin".into()),
                "auth.setup".into(),
                Some("admin".into()),
                201
            )
        );
    }

    #[tokio::test]
    async fn protected_routes_need_a_session() {
        let app = test_app(false).await;
        let routes = [
            (Method::POST, "/api/v1/auth/logout"),
            (Method::GET, "/api/v1/auth/me"),
            (Method::POST, "/api/v1/auth/password"),
            (Method::GET, "/api/v1/host"),
            (Method::GET, "/api/v1/host/stats"),
            (Method::GET, "/api/v1/host/capabilities"),
            (Method::GET, "/api/v1/containers"),
            (Method::POST, "/api/v1/containers"),
            (Method::GET, "/api/v1/containers/web"),
            (Method::DELETE, "/api/v1/containers/web"),
            (Method::POST, "/api/v1/containers/web/start"),
            (Method::POST, "/api/v1/containers/web/stop"),
            (Method::POST, "/api/v1/containers/web/restart"),
            (Method::GET, "/api/v1/containers/web/console"),
            (Method::GET, "/api/v1/storage"),
            (Method::GET, "/api/v1/templates"),
            (Method::GET, "/api/v1/jobs"),
            (
                Method::GET,
                "/api/v1/jobs/0192f0c4-0000-7000-8000-000000000000",
            ),
            (
                Method::GET,
                "/api/v1/jobs/0192f0c4-0000-7000-8000-000000000000/events",
            ),
        ];
        let forged = format!("arilbox_session={}", "a".repeat(64));
        for (method, uri) in routes {
            for cookie in [None, Some("arilbox_session=garbage"), Some(forged.as_str())] {
                let r = call(&app.anon, method.clone(), uri, None, cookie).await;
                assert_eq!(
                    (r.status, r.body["error"]["code"].as_str()),
                    (StatusCode::UNAUTHORIZED, Some("unauthenticated")),
                    "{method} {uri} {cookie:?}"
                );
                if cookie.is_some() {
                    assert!(set_cookie(&r).contains("Max-Age=0"), "stale cookie cleared");
                }
            }
        }
        // Public routes.
        let r = call(&app.anon, Method::GET, "/api/v1/health", None, None).await;
        assert_eq!(r.status, StatusCode::OK);
        // The admin session works.
        let r = call(
            &app.anon,
            Method::GET,
            "/api/v1/containers",
            None,
            Some(&app.cookie),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn login_logout_and_audit() {
        let app = test_app(false).await;
        let login = |password: &str| json!({ "username": "admin", "password": password });

        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/login",
            Some(login("wrong password")),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("invalid_credentials"))
        );
        assert!(!r.headers.contains_key("set-cookie"));

        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/login",
            Some(login(PASSWORD)),
            None,
        )
        .await;
        assert_eq!(
            (r.status, &r.body),
            (StatusCode::OK, &json!({ "username": "admin" }))
        );
        let cookie = cookie_pair(&set_cookie(&r));

        // CSRF applies to login and logout too.
        let res = app
            .anon
            .clone()
            .oneshot(
                Request::post("/api/v1/auth/logout")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/logout",
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
        assert!(set_cookie(&r).contains("Max-Age=0"));
        let r = call(
            &app.anon,
            Method::GET,
            "/api/v1/auth/me",
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "session ended");

        // A container action is attributed to the user.
        let r = call(
            &app.router,
            Method::POST,
            "/api/v1/containers/nope/start",
            None,
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND);

        let rows = audit(&app).await;
        let tail: Vec<_> = rows.iter().rev().take(5).rev().cloned().collect();
        assert_eq!(
            tail,
            [
                (None, "auth.login".into(), Some("admin".into()), 401),
                (
                    Some("admin".into()),
                    "auth.login".into(),
                    Some("admin".into()),
                    200
                ),
                (None, "auth.logout".into(), None, 403),
                (Some("admin".into()), "auth.logout".into(), None, 204),
                (
                    Some("admin".into()),
                    "container.start".into(),
                    Some("nope".into()),
                    404
                ),
            ]
        );
    }

    #[tokio::test]
    async fn login_throttling_sets_retry_after() {
        let app = test_app(false).await;
        let body = json!({ "username": "admin", "password": "wrong password" });
        for _ in 0..5 {
            let r = call(
                &app.anon,
                Method::POST,
                "/api/v1/auth/login",
                Some(body.clone()),
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        }
        let ok = json!({ "username": "admin", "password": PASSWORD });
        let r = call(
            &app.anon,
            Method::POST,
            "/api/v1/auth/login",
            Some(ok),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("too_many_attempts"))
        );
        let wait: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
        assert!((1..=900).contains(&wait), "{wait}");
    }

    #[tokio::test]
    async fn change_password_over_http() {
        let app = test_app(false).await;
        let r = call(
            &app.router,
            Method::POST,
            "/api/v1/auth/password",
            Some(json!({ "current": "nope", "new": "another password" })),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.body["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("wrong_password"))
        );
        let r = call(
            &app.router,
            Method::POST,
            "/api/v1/auth/password",
            Some(json!({ "current": PASSWORD, "new": "another password" })),
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
        let r = call(&app.router, Method::GET, "/api/v1/auth/me", None, None).await;
        assert_eq!(r.status, StatusCode::OK, "current session survives");
    }

    #[tokio::test]
    async fn responses_carry_security_headers() {
        let app = test_app(false).await;
        for (router, uri) in [
            (&app.router, "/api/v1/containers"),
            (&app.anon, "/api/v1/containers"),
        ] {
            let r = call(router, Method::GET, uri, None, None).await;
            assert!(
                r.headers["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
            assert_eq!(r.headers["x-content-type-options"], "nosniff");
            assert_eq!(r.headers["x-frame-options"], "DENY");
            assert_eq!(r.headers["referrer-policy"], "no-referrer");
            assert_eq!(r.headers["cache-control"], "no-store");
        }
    }
}
