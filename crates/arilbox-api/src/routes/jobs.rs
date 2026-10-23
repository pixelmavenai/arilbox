//! `/api/v1/jobs` (`SPEC.md` §7.5).

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;
use crate::extract::query;
use crate::jobs::{JobEvent, JobRecord, Jobs};

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 500;

fn not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "job_not_found", "Job not found")
}

fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse().map_err(|_| not_found())
}

fn db_error(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "jobs database error");
    ApiError::internal()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    limit: Option<u32>,
}

/// `GET /jobs?limit=50` — newest first.
pub(crate) async fn list(
    State(state): State<AppState>,
    uri: Uri,
) -> Result<Json<Vec<JobRecord>>, ApiError> {
    let q: ListQuery = query(&uri)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    Ok(Json(state.jobs.list(limit).await.map_err(db_error)?))
}

/// `GET /jobs/{id}`
pub(crate) async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<JobRecord>, ApiError> {
    let id = parse_id(&id)?;
    state
        .jobs
        .get(id)
        .await
        .map_err(db_error)?
        .map(Json)
        .ok_or_else(not_found)
}

/// `GET /jobs/{id}/events` — Server-Sent Events. The first event is `snapshot` (the whole
/// job); then `step_started`, `log`, `step_finished` and finally `job_finished`, after which
/// the stream ends. A finished job yields only the snapshot.
pub(crate) async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let id = parse_id(&id)?;
    let (snapshot, rx) = state
        .jobs
        .subscribe(id)
        .await
        .map_err(db_error)?
        .ok_or_else(not_found)?;
    Ok(Sse::new(event_stream(state.jobs.clone(), id, snapshot, rx))
        .keep_alive(KeepAlive::default()))
}

enum Phase {
    Snapshot(JobRecord, Option<broadcast::Receiver<JobEvent>>),
    Live(broadcast::Receiver<JobEvent>),
    Done,
}

fn snapshot_event(record: &JobRecord) -> Event {
    Event::default()
        .event("snapshot")
        .json_data(record)
        .unwrap_or_else(|_| Event::default().event("error"))
}

fn event_stream(
    jobs: Jobs,
    id: Uuid,
    snapshot: JobRecord,
    rx: Option<broadcast::Receiver<JobEvent>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(Phase::Snapshot(snapshot, rx), move |phase| {
        let jobs = jobs.clone();
        async move {
            match phase {
                Phase::Snapshot(record, rx) => {
                    let next = rx.map_or(Phase::Done, Phase::Live);
                    Some((Ok(snapshot_event(&record)), next))
                }
                Phase::Live(mut rx) => match rx.recv().await {
                    Ok(event) => {
                        let done = matches!(event, JobEvent::JobFinished { .. });
                        let sse = Event::default()
                            .event(event.name())
                            .json_data(&event)
                            .unwrap_or_else(|_| Event::default().event("error"));
                        Some((Ok(sse), if done { Phase::Done } else { Phase::Live(rx) }))
                    }
                    // Too slow to keep up: send the full state again and carry on.
                    Err(RecvError::Lagged(_)) => match jobs.get(id).await {
                        Ok(Some(record)) => Some((Ok(snapshot_event(&record)), Phase::Live(rx))),
                        _ => None,
                    },
                    Err(RecvError::Closed) => None,
                },
                Phase::Done => None,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use crate::CSRF_HEADER;
    use crate::routes::tests::{get, test_app};

    #[tokio::test]
    async fn unknown_job_is_404() {
        let app = test_app(false).await;
        for uri in [
            "/api/v1/jobs/not-a-uuid",
            "/api/v1/jobs/0192f0c4-0000-7000-8000-000000000000",
            "/api/v1/jobs/0192f0c4-0000-7000-8000-000000000000/events",
        ] {
            let res = app
                .router
                .clone()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn list_limit_validation() {
        let app = test_app(false).await;
        let (s, _, body) = get(app.router.clone(), "/api/v1/jobs").await;
        assert_eq!((s, body), (StatusCode::OK, json!([])));
        let (s, _, _) = get(app.router.clone(), "/api/v1/jobs?limit=0").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _, _) = get(app.router.clone(), "/api/v1/jobs?limit=501").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn events_stream_ends_with_job_finished() {
        let app = test_app(false).await;
        // No network: the job waits 200 ms in wait_for_network, so we subscribe while live.
        app.backend.fail_on("network");
        let res = app
            .router
            .clone()
            .oneshot(
                Request::post("/api/v1/containers")
                    .header(CSRF_HEADER, "1")
                    .body(Body::from(
                        json!({ "name": "web", "start": true }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 1 << 16).await.unwrap();
        let job_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["job_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let res = app
            .router
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/jobs/{job_id}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        // The stream closes after job_finished (or right after the snapshot if already done).
        let text = String::from_utf8(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                to_bytes(res.into_body(), 1 << 20),
            )
            .await
            .expect("stream should end")
            .unwrap()
            .to_vec(),
        )
        .unwrap();
        assert!(text.starts_with("event: snapshot\n"), "{text}");
        let order: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert_eq!(order.first(), Some(&"snapshot"));
        assert_eq!(order.last(), Some(&"job_finished"), "{text}");
        assert!(order.contains(&"step_finished"));
        assert!(text.contains("network_timeout"), "{text}");
    }
}
