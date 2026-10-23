//! `/api/v1/containers` (`SPEC.md` §7.3).
//!
//! Reads answer directly. Changes are validated here (400/404/409) and then run as jobs
//! (`202 { "job_id" }`), see [`crate::jobs::container`].

use std::time::Duration;

use arilbox_core::config::{StoragePool, TemplateRef};
use arilbox_core::{BridgeName, ContainerName, StorageName};
use arilxc::{ContainerInfo, ContainerState, CreateSpec, Limits, NetworkMode, NetworkSpec};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;
use crate::extract::{ApiJson, OptionalJson, query};
use crate::jobs::{JobSpec, TargetBusy, container as jobs};
use crate::middleware::AuditTarget;

/// Default and maximum clean-shutdown timeouts for stop/restart.
const DEFAULT_STOP_TIMEOUT_S: u64 = 30;
const MAX_STOP_TIMEOUT_S: u64 = 600;

fn bad_request(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

fn parse_name(raw: &str) -> Result<ContainerName, ApiError> {
    Ok(ContainerName::new(raw)?)
}

async fn submit(state: &AppState, job: JobSpec) -> Result<Response, ApiError> {
    let target = job.target.clone();
    match state.jobs.submit(job).await {
        Ok(id) => Ok(accepted(id, target)),
        Err(TargetBusy { job_id }) => Err(conflict(
            "target_busy",
            format!("Job {job_id} is already running for '{target}'"),
        )),
    }
}

fn accepted(id: Uuid, target: String) -> Response {
    let mut response = (StatusCode::ACCEPTED, Json(json!({ "job_id": id }))).into_response();
    response.extensions_mut().insert(AuditTarget(target));
    response
}

/// Rejects a request for a container that already has a job in progress, before any other
/// state check (its state is about to change).
fn ensure_idle(state: &AppState, name: &ContainerName) -> Result<(), ApiError> {
    if state.jobs.is_busy(name.as_str()) {
        return Err(conflict(
            "target_busy",
            format!("A job is already running for '{name}'"),
        ));
    }
    Ok(())
}

/// `GET /containers`
pub(crate) async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<ContainerInfo>>, ApiError> {
    Ok(Json(state.containers.list().await?))
}

/// `GET /containers/{name}`
pub(crate) async fn get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<ContainerInfo>, ApiError> {
    let name = parse_name(&name)?;
    Ok(Json(state.containers.info(&name).await?))
}

/// Body of `POST /containers`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    name: String,
    template: Option<TemplateRef>,
    storage: Option<String>,
    #[serde(default)]
    limits: Limits,
    network: Option<NetworkRequest>,
    #[serde(default)]
    gpu: bool,
    #[serde(default)]
    nesting: bool,
    #[serde(default)]
    autostart: bool,
    /// Start the container (and wait for its address) once created.
    #[serde(default)]
    start: bool,
    #[serde(default)]
    volumes: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkRequest {
    bridge: Option<String>,
    #[serde(default)]
    mode: NetworkMode,
}

/// `POST /containers` → 202 job.
pub(crate) async fn create(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<CreateRequest>,
) -> Result<Response, ApiError> {
    let spec = validate_create(&state, &req)?;
    ensure_idle(&state, &spec.name)?;
    match state.containers.info(&spec.name).await {
        Ok(_) => {
            return Err(conflict(
                "container_exists",
                format!("Container '{}' already exists", spec.name),
            ));
        }
        Err(arilxc::Error::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let job = jobs::create_job(spec, req.start, state.jobs.network_timeout);
    submit(&state, job).await
}

fn validate_create(state: &AppState, req: &CreateRequest) -> Result<CreateSpec, ApiError> {
    let name = ContainerName::new(req.name.as_str())?;
    let settings = &state.settings;

    let template = req
        .template
        .clone()
        .unwrap_or_else(|| settings.default_template.clone());
    if arilxc::template::image_for(&template).is_none() {
        let allowed: Vec<String> = arilxc::template::allowed_templates()
            .map(|t| format!("{}/{}", t.distro, t.release))
            .collect();
        return Err(bad_request(
            "invalid_template",
            format!("Unknown template; available: {}", allowed.join(", ")),
        ));
    }

    let storage = match &req.storage {
        Some(s) => StorageName::new(s.as_str())?,
        None => settings
            .default_storage
            .clone()
            .or_else(|| settings.storage.first().map(|p| p.name().clone()))
            .ok_or_else(|| bad_request("invalid_storage", "No storage pool is configured"))?,
    };
    let pool = settings
        .storage
        .iter()
        .find(|p| p.name() == &storage)
        .ok_or_else(|| {
            bad_request(
                "invalid_storage",
                format!("Unknown storage pool '{storage}'"),
            )
        })?;
    if matches!(pool, StoragePool::Zfs { .. }) && !state.host.capabilities().zfs {
        return Err(conflict(
            "storage_unavailable",
            format!("Storage pool '{storage}' needs ZFS, which is not available on this host"),
        ));
    }

    req.limits
        .validate()
        .map_err(|reason| bad_request("invalid_limits", reason))?;

    let (bridge, mode) = match &req.network {
        Some(n) => (n.bridge.clone(), n.mode),
        None => (None, NetworkMode::Dhcp),
    };
    let bridge = match bridge {
        Some(b) => BridgeName::new(b)?,
        None => settings.managed_bridge.clone(),
    };
    if bridge != settings.managed_bridge && !settings.sys.is_bridge(&bridge) {
        return Err(bad_request(
            "invalid_bridge",
            format!("'{bridge}' is not a bridge on this host"),
        ));
    }

    if req.gpu {
        if !state.host.capabilities().nvidia {
            return Err(conflict(
                "gpu_unavailable",
                "No usable NVIDIA GPU on this host",
            ));
        }
        return Err(bad_request(
            "unsupported",
            "GPU sharing is not available yet",
        ));
    }
    if !req.volumes.is_empty() {
        return Err(bad_request("unsupported", "Volumes are not available yet"));
    }

    Ok(CreateSpec {
        name,
        template,
        storage,
        limits: req.limits,
        network: NetworkSpec { bridge, mode },
        gpu: false,
        nesting: req.nesting,
        autostart: req.autostart,
    })
}

/// Query of `DELETE /containers/{name}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteQuery {
    /// Volumes arrive in M4; accepted now so clients don't break later.
    #[serde(default)]
    #[allow(dead_code)]
    keep_volumes: bool,
    #[serde(default)]
    force: bool,
}

/// `DELETE /containers/{name}?keep_volumes=bool&force=bool` → 202 job.
pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    uri: Uri,
) -> Result<Response, ApiError> {
    let name = parse_name(&name)?;
    let q: DeleteQuery = query(&uri)?;
    ensure_idle(&state, &name)?;
    let info = state.containers.info(&name).await?;
    if info.state != ContainerState::Stopped && !q.force {
        return Err(conflict(
            "container_running",
            format!(
                "Container '{name}' is {}; stop it first or pass force=true",
                info.state
            ),
        ));
    }
    submit(&state, jobs::delete_job(name, q.force)).await
}

/// `POST /containers/{name}/start` → 202 job.
pub(crate) async fn start(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let name = parse_name(&name)?;
    ensure_idle(&state, &name)?;
    let info = state.containers.info(&name).await?;
    if info.state != ContainerState::Stopped {
        return Err(arilxc::Error::WrongState {
            name,
            state: info.state,
        }
        .into());
    }
    submit(&state, jobs::start_job(name, state.jobs.network_timeout)).await
}

/// Body of `POST /containers/{name}/stop` and `/restart`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopRequest {
    #[serde(default = "default_stop_timeout")]
    timeout_s: u64,
}

fn default_stop_timeout() -> u64 {
    DEFAULT_STOP_TIMEOUT_S
}

impl Default for StopRequest {
    fn default() -> Self {
        Self {
            timeout_s: DEFAULT_STOP_TIMEOUT_S,
        }
    }
}

impl StopRequest {
    fn timeout(&self) -> Result<Duration, ApiError> {
        if !(1..=MAX_STOP_TIMEOUT_S).contains(&self.timeout_s) {
            return Err(bad_request(
                "invalid_request",
                format!("timeout_s must be between 1 and {MAX_STOP_TIMEOUT_S}"),
            ));
        }
        Ok(Duration::from_secs(self.timeout_s))
    }
}

/// `POST /containers/{name}/stop` `{ "timeout_s": 30 }` → 202 job.
pub(crate) async fn stop(
    State(state): State<AppState>,
    Path(name): Path<String>,
    OptionalJson(req): OptionalJson<StopRequest>,
) -> Result<Response, ApiError> {
    let name = parse_name(&name)?;
    let timeout = req.timeout()?;
    ensure_idle(&state, &name)?;
    let info = state.containers.info(&name).await?;
    if info.state != ContainerState::Running {
        return Err(arilxc::Error::WrongState {
            name,
            state: info.state,
        }
        .into());
    }
    submit(&state, jobs::stop_job(name, timeout)).await
}

/// `POST /containers/{name}/restart` `{ "timeout_s": 30 }` → 202 job. A stopped container is
/// simply started.
pub(crate) async fn restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
    OptionalJson(req): OptionalJson<StopRequest>,
) -> Result<Response, ApiError> {
    let name = parse_name(&name)?;
    let timeout = req.timeout()?;
    ensure_idle(&state, &name)?;
    // 404 before accepting the job.
    state.containers.info(&name).await?;
    submit(
        &state,
        jobs::restart_job(name, timeout, state.jobs.network_timeout),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use arilxc::ContainerBackend;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use serde_json::{Value, json};
    use sqlx::Row;
    use tower::ServiceExt;

    use crate::CSRF_HEADER;
    use crate::routes::tests::{TestApp, test_app};

    async fn send(
        app: &TestApp,
        method: Method,
        uri: &str,
        body: Option<Value>,
        csrf: bool,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if csrf {
            req = req.header(CSRF_HEADER, "1");
        }
        let body = match body {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = app
            .router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, json)
    }

    async fn post(app: &TestApp, uri: &str, body: Value) -> (StatusCode, Value) {
        send(app, Method::POST, uri, Some(body), true).await
    }

    /// Waits for a job and returns it as JSON.
    async fn finished(app: &TestApp, job_id: &Value) -> Value {
        let uri = format!("/api/v1/jobs/{}", job_id.as_str().unwrap());
        for _ in 0..300 {
            let (status, job) = send(app, Method::GET, &uri, None, false).await;
            assert_eq!(status, StatusCode::OK);
            if job["status"] == "succeeded" || job["status"] == "failed" {
                // Let the runner release the target.
                tokio::time::sleep(Duration::from_millis(20)).await;
                return job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job did not finish");
    }

    async fn create_running(app: &TestApp, name: &str) {
        let (status, body) = post(
            app,
            "/api/v1/containers",
            json!({ "name": name, "start": true }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let job = finished(app, &body["job_id"]).await;
        assert_eq!(job["status"], "succeeded", "{job}");
    }

    async fn audit_rows(app: &TestApp) -> Vec<(String, Option<String>, i64)> {
        sqlx::query("SELECT action, target, status FROM audit_log ORDER BY id")
            .fetch_all(&app.state.db)
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get("action"), r.get("target"), r.get("status")))
            .collect()
    }

    #[tokio::test]
    async fn create_runs_a_job_and_lists_the_container() {
        let app = test_app(false).await;
        let (status, body) = post(
            &app,
            "/api/v1/containers",
            json!({
                "name": "ai",
                "template": { "distro": "debian", "release": "13" },
                "storage": "local-dir",
                "limits": { "cores": 4, "memory_mb": 8192, "swap_mb": 0, "disk_gb": 50 },
                "network": { "bridge": "arilbox0", "mode": "dhcp" },
                "nesting": true,
                "autostart": true,
                "start": true
            }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let job = finished(&app, &body["job_id"]).await;
        assert_eq!(job["kind"], "container_create");
        assert_eq!(job["target"], "ai");
        assert_eq!(job["status"], "succeeded");

        let (status, list) = send(&app, Method::GET, "/api/v1/containers", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list[0]["name"], "ai");
        assert_eq!(list[0]["state"], "running");

        let (status, one) = send(&app, Method::GET, "/api/v1/containers/ai", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            one["template"],
            json!({ "distro": "debian", "release": "13" })
        );

        let limits = app
            .backend
            .limits(&arilbox_core::ContainerName::new("ai").unwrap())
            .unwrap();
        assert_eq!(limits.cores, Some(4));

        assert_eq!(
            audit_rows(&app).await,
            [("container.create".into(), Some("ai".into()), 202)]
        );
    }

    #[tokio::test]
    async fn create_validation_errors() {
        let app = test_app(false).await;
        let cases = [
            (
                json!({ "name": "Bad_Name" }),
                StatusCode::BAD_REQUEST,
                "invalid_name",
            ),
            (
                json!({ "name": "host" }),
                StatusCode::BAD_REQUEST,
                "invalid_name",
            ),
            (
                json!({ "name": "a", "bogus": 1 }),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                json!({ "name": 5 }),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                json!({ "name": "a", "limits": { "cores": 0 } }),
                StatusCode::BAD_REQUEST,
                "invalid_limits",
            ),
            (
                json!({ "name": "a", "limits": { "ram": 1 } }),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
            (
                json!({ "name": "a", "template": { "distro": "debian", "release": "12" } }),
                StatusCode::BAD_REQUEST,
                "invalid_template",
            ),
            (
                json!({ "name": "a", "storage": "nope" }),
                StatusCode::BAD_REQUEST,
                "invalid_storage",
            ),
            (
                json!({ "name": "a", "storage": "local-zfs" }),
                StatusCode::CONFLICT,
                "storage_unavailable",
            ),
            (
                json!({ "name": "a", "network": { "bridge": "vmbr0" } }),
                StatusCode::BAD_REQUEST,
                "invalid_bridge",
            ),
            (
                json!({ "name": "a", "gpu": true }),
                StatusCode::CONFLICT,
                "gpu_unavailable",
            ),
            (
                json!({ "name": "a", "volumes": [{ "name": "data" }] }),
                StatusCode::BAD_REQUEST,
                "unsupported",
            ),
        ];
        for (body, status, code) in &cases {
            let (s, b) = post(&app, "/api/v1/containers", body.clone()).await;
            assert_eq!(
                (s, b["error"]["code"].as_str()),
                (*status, Some(*code)),
                "{body}"
            );
        }
        assert!(app.backend.list().await.unwrap().is_empty());
        // Rejected requests are audited too.
        assert_eq!(audit_rows(&app).await.len(), cases.len());
    }

    #[tokio::test]
    async fn malformed_json_is_400() {
        let app = test_app(false).await;
        let res = app
            .router
            .clone()
            .oneshot(
                Request::post("/api/v1/containers")
                    .header(CSRF_HEADER, "1")
                    .body(Body::from("{not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_conflicts() {
        let app = test_app(false).await;
        create_running(&app, "web").await;
        let (s, b) = post(&app, "/api/v1/containers", json!({ "name": "web" })).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("container_exists"))
        );
    }

    #[tokio::test]
    async fn mutations_require_csrf_header() {
        let app = test_app(false).await;
        create_running(&app, "web").await;
        for (method, uri) in [
            (Method::POST, "/api/v1/containers"),
            (Method::DELETE, "/api/v1/containers/web"),
            (Method::POST, "/api/v1/containers/web/start"),
            (Method::POST, "/api/v1/containers/web/stop"),
            (Method::POST, "/api/v1/containers/web/restart"),
        ] {
            let (s, b) = send(
                &app,
                method.clone(),
                uri,
                Some(json!({ "name": "x" })),
                false,
            )
            .await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(b["error"]["code"], "csrf_failed");
        }
        // Reads don't need it.
        let (s, _) = send(&app, Method::GET, "/api/v1/containers", None, false).await;
        assert_eq!(s, StatusCode::OK);
        // The rejection is audited with the path's container as target.
        let rows = audit_rows(&app).await;
        assert!(rows.contains(&("container.delete".into(), Some("web".into()), 403)));
    }

    #[tokio::test]
    async fn unknown_container_is_404() {
        let app = test_app(false).await;
        for (method, uri) in [
            (Method::GET, "/api/v1/containers/nope"),
            (Method::DELETE, "/api/v1/containers/nope"),
            (Method::POST, "/api/v1/containers/nope/start"),
            (Method::POST, "/api/v1/containers/nope/stop"),
            (Method::POST, "/api/v1/containers/nope/restart"),
        ] {
            let (s, b) = send(&app, method.clone(), uri, None, true).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{method} {uri}");
            assert_eq!(b["error"]["code"], "container_not_found");
        }
    }

    #[tokio::test]
    async fn invalid_name_in_path_is_400() {
        let app = test_app(false).await;
        let (s, b) = send(&app, Method::GET, "/api/v1/containers/-rf", None, false).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_name"))
        );
    }

    #[tokio::test]
    async fn lifecycle_and_state_conflicts() {
        let app = test_app(false).await;
        create_running(&app, "web").await;

        // Running: start and plain delete conflict.
        let (s, b) = post(&app, "/api/v1/containers/web/start", json!({})).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("container_running"))
        );
        let (s, b) = send(&app, Method::DELETE, "/api/v1/containers/web", None, true).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("container_running"))
        );

        // Stop with a body, then stop again conflicts.
        let (s, b) = post(
            &app,
            "/api/v1/containers/web/stop",
            json!({ "timeout_s": 5 }),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        assert_eq!(finished(&app, &b["job_id"]).await["status"], "succeeded");
        let (s, b) = send(
            &app,
            Method::POST,
            "/api/v1/containers/web/stop",
            None,
            true,
        )
        .await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("container_stopped"))
        );

        // Bad stop body.
        let (s, _) = post(
            &app,
            "/api/v1/containers/web/restart",
            json!({ "timeout_s": 0 }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = post(
            &app,
            "/api/v1/containers/web/restart",
            json!({ "timeout": 5 }),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        // Restart a stopped container = start; no body needed.
        let (s, b) = send(
            &app,
            Method::POST,
            "/api/v1/containers/web/restart",
            None,
            true,
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        assert_eq!(finished(&app, &b["job_id"]).await["status"], "succeeded");

        // Forced delete of a running container.
        let (s, b) = send(
            &app,
            Method::DELETE,
            "/api/v1/containers/web?force=true&keep_volumes=false",
            None,
            true,
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{b}");
        assert_eq!(finished(&app, &b["job_id"]).await["status"], "succeeded");
        let (s, _) = send(&app, Method::GET, "/api/v1/containers/web", None, false).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let (s, _) = send(
            &app,
            Method::DELETE,
            "/api/v1/containers/web?bogus=1",
            None,
            true,
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn busy_target_is_409() {
        let app = test_app(false).await;
        // Network never comes up, so the create job sits in wait_for_network for 200 ms.
        app.backend.fail_on("network");
        let (s, _) = post(
            &app,
            "/api/v1/containers",
            json!({ "name": "web", "start": true }),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        let (s, b) = post(&app, "/api/v1/containers", json!({ "name": "web" })).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("target_busy"))
        );
        let (s, b) = post(&app, "/api/v1/containers/web/stop", json!({})).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("target_busy"))
        );
    }
}
